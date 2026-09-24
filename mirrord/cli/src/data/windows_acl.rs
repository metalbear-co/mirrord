//! Restricts a directory to the current user on Windows, the counterpart of `0700` on unix.

use std::{io, mem::size_of, os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};

use winapi::{
    shared::{minwindef::DWORD, winerror::ERROR_SUCCESS},
    um::{
        accctrl::SE_FILE_OBJECT,
        aclapi::SetNamedSecurityInfoW,
        handleapi::CloseHandle,
        processthreadsapi::{GetCurrentProcess, OpenProcessToken},
        securitybaseapi::{
            AddAccessAllowedAceEx, GetLengthSid, GetTokenInformation, InitializeAcl,
        },
        winnt::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, CONTAINER_INHERIT_ACE,
            DACL_SECURITY_INFORMATION, FILE_ALL_ACCESS, HANDLE, OBJECT_INHERIT_ACE,
            PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
    },
};

/// Replaces the DACL of the directory at `path` with one that grants access only to the user
/// running this process, and that files and directories created inside inherit.
///
/// The DACL is protected, so it does not inherit the ACEs of the user's profile directory, which
/// usually grant access to `SYSTEM` and administrators as well. Setting it also propagates the
/// new inheritable ACE to existing children that inherit their ACLs.
pub(super) fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    let token_user = current_token_user()?;
    // SAFETY: `token_user` holds a `TOKEN_USER` written by `GetTokenInformation`, and is aligned
    // for it.
    let sid = unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // SAFETY: `sid` points into `token_user`, which is still alive.
    let sid_length = unsafe { GetLengthSid(sid) } as usize;

    let acl_size =
        size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<DWORD>() + sid_length;
    // `u32` elements, since an ACL must be `DWORD`-aligned.
    let mut acl_buffer = vec![0u32; acl_size.div_ceil(size_of::<u32>())];
    let acl = acl_buffer.as_mut_ptr().cast::<ACL>();

    // SAFETY: `acl` points to a buffer of at least `acl_size` bytes.
    if unsafe { InitializeAcl(acl, acl_size as DWORD, ACL_REVISION as DWORD) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `acl` was initialized with room for exactly this ACE, and `sid` is valid.
    if unsafe {
        AddAccessAllowedAceEx(
            acl,
            ACL_REVISION as DWORD,
            (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as DWORD,
            FILE_ALL_ACCESS,
            sid,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide_path` is NUL-terminated, and `acl` is a valid ACL. The owner, group and SACL
    // are not set, so their null pointers are not read.
    let result = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null_mut(),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }

    Ok(())
}

/// Returns a buffer holding the `TOKEN_USER` of this process's token.
///
/// `u64` elements, since `TOKEN_USER` holds a pointer and must be aligned for it.
fn current_token_user() -> io::Result<Vec<u64>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = TokenHandle(token);

    let mut needed: DWORD = 0;
    // SAFETY: A null buffer of length 0 only queries the required size into `needed`.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) };

    let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `needed` bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    Ok(buffer)
}

/// Closes the token handle on drop.
struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        // SAFETY: The handle was opened by `OpenProcessToken` and is closed only here.
        unsafe { CloseHandle(self.0) };
    }
}
