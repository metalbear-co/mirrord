//! Builds a [`SECURITY_ATTRIBUTES`] whose DACL grants the current process's user full access
//! to a named pipe and denies access to everyone else.
//!
//! Without this, named pipes default to a DACL that allows read access to the `Everyone`
//! group, so any logged-in user could subscribe to the session's `/events` stream. The session
//! monitor surfaces process names, file paths, env-var names and other potentially sensitive
//! material — confidentiality is the same contract as `0o600` on the unix socket.

#![cfg(windows)]

use std::{io, mem::size_of, ptr::null_mut};

use winapi::{
    shared::minwindef::{DWORD, FALSE, TRUE},
    um::{
        handleapi::CloseHandle,
        minwinbase::SECURITY_ATTRIBUTES,
        processthreadsapi::{GetCurrentProcess, OpenProcessToken},
        securitybaseapi::{
            AddAccessAllowedAce, GetLengthSid, GetTokenInformation, InitializeAcl,
            InitializeSecurityDescriptor, SetSecurityDescriptorDacl,
        },
        winnt::{
            ACL, ACL_REVISION, GENERIC_ALL, HANDLE, PSID, SECURITY_DESCRIPTOR_MIN_LENGTH,
            SECURITY_DESCRIPTOR_REVISION, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
    },
};

/// Bundle of buffers backing a [`SECURITY_ATTRIBUTES`] whose DACL only allows access to the
/// current process's user. The buffers are kept on this struct because the SA holds raw
/// pointers into them — moving the struct is fine (the heap allocations don't move with it),
/// but dropping it invalidates the SA.
pub struct PipeSecurity {
    _token_user_buffer: Vec<u8>,
    _acl_buffer: Vec<u8>,
    _sd_buffer: Vec<u8>,
    sa: SECURITY_ATTRIBUTES,
}

// SAFETY: The pointer inside `sa.lpSecurityDescriptor` points into `_sd_buffer`, which is
// owned by `PipeSecurity` and follows it across thread boundaries. No other thread observes
// the pointer concurrently because we only expose it via `as_raw` to a synchronous winapi
// call that copies the descriptor into the kernel-side pipe handle.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// Constructs a [`PipeSecurity`] for the calling process's user.
    pub fn for_current_user() -> io::Result<Self> {
        unsafe { build_security_attributes() }
    }

    /// Pointer to the [`SECURITY_ATTRIBUTES`] suitable for passing to
    /// `ServerOptions::create_with_security_attributes_raw`. Valid only while `self` is alive.
    pub fn as_raw(&self) -> *mut std::ffi::c_void {
        &self.sa as *const SECURITY_ATTRIBUTES as *mut std::ffi::c_void
    }
}

unsafe fn build_security_attributes() -> io::Result<PipeSecurity> {
    // 1) Open the current process token so we can query the user SID.
    let mut token: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token_guard = TokenHandle(token);

    // 2) GetTokenInformation called twice: first to learn the buffer size, then for real.
    let mut needed: DWORD = 0;
    unsafe {
        GetTokenInformation(token_guard.0, TokenUser, null_mut(), 0, &mut needed);
    }
    let mut token_user_buffer = vec![0u8; needed as usize];
    if unsafe {
        GetTokenInformation(
            token_guard.0,
            TokenUser,
            token_user_buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let token_user = unsafe { &*(token_user_buffer.as_ptr() as *const TOKEN_USER) };
    let sid: PSID = token_user.User.Sid;
    let sid_length = unsafe { GetLengthSid(sid) };

    // 3) Build a small ACL holding one ACCESS_ALLOWED_ACE for the user SID.
    // `sizeof(ACL) + sizeof(ACCESS_ALLOWED_ACE) - sizeof(DWORD) + GetLengthSid()` is the
    // tight size; we add slack to accommodate any winapi struct alignment.
    let acl_size = (size_of::<ACL>() + 16 + sid_length as usize) as DWORD;
    let mut acl_buffer = vec![0u8; acl_size as usize];
    let acl_ptr = acl_buffer.as_mut_ptr() as *mut ACL;

    if unsafe { InitializeAcl(acl_ptr, acl_size, ACL_REVISION as DWORD) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { AddAccessAllowedAce(acl_ptr, ACL_REVISION as DWORD, GENERIC_ALL, sid) } == 0 {
        return Err(io::Error::last_os_error());
    }

    // 4) Build the security descriptor and attach the DACL.
    let mut sd_buffer = vec![0u8; SECURITY_DESCRIPTOR_MIN_LENGTH];
    let sd_ptr = sd_buffer.as_mut_ptr().cast();
    if unsafe { InitializeSecurityDescriptor(sd_ptr, SECURITY_DESCRIPTOR_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { SetSecurityDescriptorDacl(sd_ptr, TRUE, acl_ptr, FALSE) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as DWORD,
        lpSecurityDescriptor: sd_ptr,
        bInheritHandle: FALSE,
    };

    drop(token_guard);

    Ok(PipeSecurity {
        _token_user_buffer: token_user_buffer,
        _acl_buffer: acl_buffer,
        _sd_buffer: sd_buffer,
        sa,
    })
}

struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
