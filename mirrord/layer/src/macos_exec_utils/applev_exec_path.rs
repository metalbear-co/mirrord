//! MacOS puts the current executable's path on the stack after the argv and env in this barely
//! documented struct called applev - see:
//! https://github.com/apple/darwin-xnu/blob/2ff845c2e033bd0ff64b5b6aa6063a1f8f65aa32/bsd/kern/kern_exec.c#L4940-L4941
//! Go uses the path from there to initialize a variable that it in turns uses in `os.Executable`
//! on macOS.
//! Here we have `strip_mirrord_sip_dir_from_exec_path_in_memory` which modifies that path in place,
//! so that it is not a path inside our SIP patch directory, but rather always the original binary.

use std::{ffi::CStr, ptr};

use mirrord_sip::MIRRORD_PATCH_DIR;

extern "C" {
    static environ: *const *const i8;
}

const EXEC_PATH_PREFIX: &str = "executable_path=";

/// Get a mutable pointer to the exec path.
/// After envv, after the "executable_path=" prefix.
fn get_exec_path() -> *mut i8 {
    unsafe {
        let mut env_ptr = environ;

        // Skip past all environment variables to find the end
        while !(*env_ptr).is_null() {
            env_ptr = env_ptr.add(1);
        }

        // Skip past the null terminator
        env_ptr = env_ptr.add(1);

        let path_ptr = *(env_ptr as *const *mut i8);

        // Skip past the "executable_path=" prefix
        path_ptr.add(EXEC_PATH_PREFIX.len())
    }
}

/// On macOS, the path of the current executable is present in the memory after argv and env.
/// Go sometimes uses the path from that location in order to derive the GOROOT path relative to
/// the `go` binary (not the user's go program, the CLI tool `go`). If the current executable was
/// patched for SIP (happens if go was installed from the installer on the go website), the path
/// will be inside our temporary directory for SIP-patched binaries, and go will fail to find the
/// files it is looking for relative to it.
/// So we change the path in the user's process's memory, so that even if it uses Go to find the
/// executable's path, it gets the original one, not the SIP-patched one.
pub(crate) fn strip_mirrord_sip_dir_from_exec_path_in_memory() {
    let exec_path_ptr = get_exec_path();
    let path_cstr = unsafe { CStr::from_ptr(exec_path_ptr) };

    tracing::trace!(
        "Current executable path from applev: {}",
        path_cstr.to_string_lossy()
    );

    let path_bytes = path_cstr.to_bytes();
    let mirrord_dir_path = MIRRORD_PATCH_DIR.as_bytes();

    // Find the substring in the bytes
    if let Some(offset) = path_bytes
        .windows(mirrord_dir_path.len())
        .position(|window| window == mirrord_dir_path)
    {
        tracing::trace!(%offset, "The current executable's path contained mirrord's dir.");
        let new_len = path_bytes.len() - offset;
        unsafe {
            // The path is overlapping, don't change this to non_overlapping.
            ptr::copy(
                exec_path_ptr.add(offset + mirrord_dir_path.len()),
                exec_path_ptr,
                new_len,
            );
            *(exec_path_ptr.add(new_len)) = b'\0' as i8;
        }
    } else {
        tracing::trace!("The current executable's path does not contain mirrord's dir");
    }
}
