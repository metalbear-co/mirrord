//! Current-user-only access control for Windows objects, the counterpart of `0o600`/`0o700` on
//! unix.
//!
//! Objects created with default security are usually readable by other users on the machine:
//! named pipes grant read access to `Everyone`, and files inherit the ACEs of their parent
//! directory, which often include other users as well. mirrord keeps session data and
//! credentials in such objects, so it gives them a DACL holding a single ACE for the user running
//! this process instead.
//!
//! [`CurrentUserSecurityAttributes`] applies that DACL to objects created through APIs that take a
//! [`SECURITY_ATTRIBUTES`], like named pipes. [`restrict_path_to_current_user`] applies it to an
//! existing file or directory.

use std::{ffi::c_void, io, mem::size_of, os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};

use winapi::{
    shared::{
        minwindef::{DWORD, FALSE, TRUE},
        winerror::ERROR_SUCCESS,
    },
    um::{
        accctrl::SE_FILE_OBJECT,
        aclapi::SetNamedSecurityInfoW,
        handleapi::CloseHandle,
        minwinbase::SECURITY_ATTRIBUTES,
        processthreadsapi::{GetCurrentProcess, OpenProcessToken},
        securitybaseapi::{
            AddAccessAllowedAceEx, GetLengthSid, GetTokenInformation, InitializeAcl,
            InitializeSecurityDescriptor, SetSecurityDescriptorDacl,
        },
        winnt::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, CONTAINER_INHERIT_ACE,
            DACL_SECURITY_INFORMATION, FILE_ALL_ACCESS, GENERIC_ALL, HANDLE, OBJECT_INHERIT_ACE,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_DESCRIPTOR,
            SECURITY_DESCRIPTOR_REVISION, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
    },
};

/// A [`SECURITY_ATTRIBUTES`] whose DACL grants full access to the user running this process, and
/// no access to anyone else.
///
/// The attributes point into the descriptor and ACL owned by this struct. Both live on the heap,
/// so moving the struct is fine, but dropping it invalidates the pointer returned by
/// [`Self::as_raw`].
pub struct CurrentUserSecurityAttributes {
    _acl: CurrentUserAcl,
    _descriptor: Box<SECURITY_DESCRIPTOR>,
    attributes: SECURITY_ATTRIBUTES,
}

// SAFETY: The raw pointers inside `attributes` point into the heap allocations owned by this
// struct, which follow it across threads. Nothing writes through them after construction.
unsafe impl Send for CurrentUserSecurityAttributes {}
// SAFETY: See the `Send` impl. The pointers are only ever read, by winapi calls that copy the
// descriptor into the object they create.
unsafe impl Sync for CurrentUserSecurityAttributes {}

impl CurrentUserSecurityAttributes {
    pub fn new() -> io::Result<Self> {
        let acl = CurrentUserAcl::new(GENERIC_ALL, 0)?;

        let mut descriptor = Box::<SECURITY_DESCRIPTOR>::default();
        let descriptor_ptr: PSECURITY_DESCRIPTOR = (&raw mut *descriptor).cast();
        // SAFETY: `descriptor_ptr` points to a `SECURITY_DESCRIPTOR`.
        if unsafe { InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The descriptor was initialized above. It only stores a pointer to `acl`, and
        // both end up owned by the returned struct.
        if unsafe { SetSecurityDescriptorDacl(descriptor_ptr, TRUE, acl.as_ptr(), FALSE) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lpSecurityDescriptor: descriptor_ptr,
            bInheritHandle: FALSE,
        };

        Ok(Self {
            _acl: acl,
            _descriptor: descriptor,
            attributes,
        })
    }

    /// Pointer to the [`SECURITY_ATTRIBUTES`], e.g. for
    /// `ServerOptions::create_with_security_attributes_raw`. Valid only while `self` is alive.
    pub fn as_raw(&self) -> *mut c_void {
        (&raw const self.attributes).cast_mut().cast()
    }
}

/// Replaces the DACL of the file or directory at `path` with one that grants access only to the
/// user running this process, and that files and directories created inside inherit.
///
/// The DACL is protected, so it does not inherit the ACEs of the parent directory, which for the
/// user's profile usually grant access to `SYSTEM` and administrators as well. Setting it also
/// propagates the new inheritable ACE to existing children that inherit their ACLs.
pub fn restrict_path_to_current_user(path: &Path) -> io::Result<()> {
    let acl = CurrentUserAcl::new(
        FILE_ALL_ACCESS,
        (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as DWORD,
    )?;

    // For `SE_FILE_OBJECT`, `SetNamedSecurityInfoW` takes a local or UNC path to the file or
    // directory, which is what `Path` holds.
    // See https://learn.microsoft.com/en-us/windows/win32/api/accctrl/ne-accctrl-se_object_type
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
            acl.as_ptr(),
            null_mut(),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }

    Ok(())
}

/// An ACL holding a single ACE that allows `access_mask` to the user running this process.
///
/// `u32` elements, since an ACL must be `DWORD`-aligned.
struct CurrentUserAcl(Vec<u32>);

impl CurrentUserAcl {
    fn new(access_mask: DWORD, ace_flags: DWORD) -> io::Result<Self> {
        let token_user = CurrentTokenUser::query()?;
        let sid = token_user.sid();
        // SAFETY: `sid` points into `token_user`, which is still alive.
        let sid_length = unsafe { GetLengthSid(sid) } as usize;

        let acl_size =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<DWORD>() + sid_length;
        let mut buffer = vec![0u32; acl_size.div_ceil(size_of::<u32>())];
        let acl = buffer.as_mut_ptr().cast::<ACL>();

        // SAFETY: `acl` points to a buffer of at least `acl_size` bytes.
        if unsafe { InitializeAcl(acl, acl_size as DWORD, ACL_REVISION as DWORD) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `acl` was initialized with room for exactly this ACE, and `sid` is valid. The
        // SID is copied into the ACE, so the ACL does not borrow `token_user`.
        if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION as DWORD, ace_flags, access_mask, sid) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }

        Ok(Self(buffer))
    }

    /// The winapi functions that take an ACL want a mutable pointer, but only read through it.
    fn as_ptr(&self) -> *mut ACL {
        self.0.as_ptr().cast_mut().cast()
    }
}

/// The `TOKEN_USER` of this process's token, which identifies the user running it.
///
/// `u64` elements, since `TOKEN_USER` holds a pointer and must be aligned for it.
struct CurrentTokenUser(Vec<u64>);

impl CurrentTokenUser {
    fn query() -> io::Result<Self> {
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

        Ok(Self(buffer))
    }

    /// The user's SID. It points into `self`.
    fn sid(&self) -> PSID {
        // SAFETY: The buffer holds a `TOKEN_USER` written by `GetTokenInformation`, and is aligned
        // for it.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// Closes the token handle on drop.
struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        // SAFETY: The handle was opened by `OpenProcessToken` and is closed only here.
        unsafe { CloseHandle(self.0) };
    }
}
