//! Shared machinery for hooking syscalls made by a Go runtime.
//!
//! Go issues syscalls directly through its own runtime instead of going through libc, so libc
//! interposition never sees them. Catching them requires replacing the Go runtime's internal
//! syscall entry points (`syscall.Syscall6` and friends) with assembly trampolines that switch to
//! the `g0` system stack and then call into ordinary C-ABI Rust code.
//!
//! That trampoline machinery is identical for every layer that wants to interpose on a Go
//! application; only the table deciding what to do with each syscall differs. This crate owns the
//! trampolines, the Go runtime version detection, and the symbol hooking, and lets each layer plug
//! in its own dispatch table through [`init`].
//!
//! # Usage
//!
//! Register the dispatch table once, before hooking, then install the hooks:
//!
//! ```no_run
//! # use mirrord_layer_core::hooks::HookManager;
//! # use mirrord_layer_go::{Handlers, passthrough};
//! # unsafe extern "C" fn syscall6(n: i64, a: i64, b: i64, c: i64, d: i64, e: i64, f: i64) -> i64 {
//! #     unsafe { passthrough(n, a, b, c, d, e, f) }
//! # }
//! # unsafe extern "C" fn syscall(n: i64, a: i64, b: i64, c: i64) -> i64 {
//! #     unsafe { passthrough(n, a, b, c, 0, 0, 0) }
//! # }
//! # let hook_manager = &mut HookManager::default();
//! mirrord_layer_go::init(Handlers { syscall, syscall6 });
//! mirrord_layer_go::enable_hooks(hook_manager, true);
//! ```
#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]

use std::{ffi::CStr, sync::OnceLock};

use frida_gum::NativePointer;
use mirrord_layer_core::hooks::HookManager;
use nix::errno::Errno;

#[cfg_attr(target_arch = "x86_64", path = "linux_x64.rs")]
#[cfg_attr(target_arch = "aarch64", path = "linux_aarch64.rs")]
mod arch;

pub use arch::{enable_hooks, enable_hooks_in_loaded_module};

/// Dispatch table for syscalls intercepted in a Go runtime.
///
/// Both entries receive the raw syscall number and arguments, and must return the raw kernel
/// result: a non-negative value on success, or `-errno` on failure. [`passthrough`] performs the
/// syscall unchanged and is the right thing to return for anything the layer does not handle.
#[derive(Clone, Copy)]
pub struct Handlers {
    /// Handles the Go runtime's 3- and 4-argument syscall entry points.
    ///
    /// Only reached from Go runtimes older than 1.19, which have separate entry points per
    /// argument count. Newer runtimes route everything through [`Handlers::syscall6`].
    pub syscall: SyscallHandler,
    /// Handles the Go runtime's 6-argument syscall entry point.
    pub syscall6: Syscall6Handler,
}

pub type SyscallHandler = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;
pub type Syscall6Handler = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64) -> i64;

static HANDLERS: OnceLock<Handlers> = OnceLock::new();

/// Registers the dispatch table used by the Go syscall trampolines.
///
/// Must be called before [`enable_hooks`], otherwise intercepted syscalls fall through to
/// [`passthrough`]. Only the first call takes effect.
pub fn init(handlers: Handlers) {
    let _ = HANDLERS.set(handlers);
}

/// Performs `syscall` unchanged, mimicking what the Go runtime would have done itself.
///
/// [`Errno`] is set on failure, so callers that inspect `errno` after dispatching observe the same
/// state a real syscall would have left behind.
///
/// # Safety
///
/// Issues an arbitrary syscall with arbitrary arguments. Callers must only forward a syscall the
/// application was already making, with its arguments untouched.
pub unsafe fn passthrough(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    let (Ok(result) | Err(result)) = unsafe {
        syscalls::syscall!(
            syscalls::Sysno::from(syscall as i32),
            param1,
            param2,
            param3,
            param4,
            param5,
            param6
        )
    }
    .map(|success| success as i64)
    .map_err(|fail| {
        let raw_errno = fail.into_raw();
        Errno::set_raw(raw_errno);

        -(raw_errno as i64)
    });

    result
}

/// C-ABI entry point called by the 6-argument assembly trampolines.
///
/// The symbol name is referenced verbatim from the `naked_asm!` blocks in the architecture
/// modules, so it must stay unmangled.
#[unsafe(no_mangle)]
unsafe extern "C" fn c_abi_syscall6_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    match HANDLERS.get() {
        Some(handlers) => unsafe {
            (handlers.syscall6)(syscall, param1, param2, param3, param4, param5, param6)
        },
        None => unsafe { passthrough(syscall, param1, param2, param3, param4, param5, param6) },
    }
}

/// C-ABI entry point called by the 3- and 4-argument assembly trampolines.
///
/// The symbol name is referenced verbatim from the `naked_asm!` blocks in the architecture
/// modules, so it must stay unmangled.
#[unsafe(no_mangle)]
unsafe extern "C" fn c_abi_syscall_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
) -> i64 {
    match HANDLERS.get() {
        Some(handlers) => unsafe { (handlers.syscall)(syscall, param1, param2, param3) },
        None => unsafe { passthrough(syscall, param1, param2, param3, 0, 0, 0) },
    }
}

/// Handler for `rawVforkSyscall` calls.
///
/// Removes the [`libc::CLONE_VM`] flag from the clone flags.
/// This way the child process will **not** share parent's memory,
/// and we will be able to safely use hooks in the child.
///
/// The [`libc::CLONE_VFORK`] flag is left intact on purpose,
/// as it only suspends the parent process until the child exits or execs
/// (which is a behavior we want to preserve - the user application might depend on it).
///
/// See [Linux manual](https://man7.org/linux/man-pages/man2/clone.2.html) for reference.
#[unsafe(no_mangle)]
unsafe extern "C" fn raw_vfork_handler(
    mut param_1: i64,
    param_2: i64,
    param_3: i64,
    syscall_num: i64,
) -> i64 {
    if syscall_num == libc::SYS_clone {
        param_1 &= !(libc::CLONE_VM as i64);
    } else if syscall_num == libc::SYS_clone3 {
        let args = param_1 as *mut libc::clone_args;
        let args = unsafe {
            // Safety: we don't validate pointers from the user app.
            args.as_mut()
        };
        if let Some(args) = args {
            args.flags &= !(libc::CLONE_VM as u64);
        }
    };

    syscalls::syscall!(
        syscalls::Sysno::from(syscall_num as i32),
        param_1,
        param_2,
        param_3,
        0,
        0,
        0
    )
    .map(|success| success as i64)
    .unwrap_or_else(|error| {
        let raw_errno = error.into_raw();
        -(raw_errno as i64)
    })
}

/// Extracts version of the Go runtime in the current process.
fn get_go_runtime_version(hook_manager: &mut HookManager) -> Option<f32> {
    let version_symbol = hook_manager.resolve_symbol_main_module("runtime.buildVersion.str")?;
    get_version_from_symbol(version_symbol)
}

/// Extracts version of the Go runtime from the given module.
fn get_go_runtime_version_in_module(
    hook_manager: &mut HookManager,
    module_name: &str,
) -> Option<f32> {
    let version_symbol =
        hook_manager.resolve_symbol_in_module(module_name, "runtime.buildVersion.str")?;
    get_version_from_symbol(version_symbol)
}

/// buildVersion can look a bit complex:
/// devel go1.25-ecc06f0 Wed Apr 9 00:32:10 2025 -0700
///
/// We need to find the word starting with 'go', and parse the next 4 characters.
fn get_version_from_symbol(version_symbol: NativePointer) -> Option<f32> {
    let version = unsafe {
        let cstr = CStr::from_ptr(version_symbol.0 as _);
        std::str::from_utf8_unchecked(cstr.to_bytes())
    };
    version
        .split_ascii_whitespace()
        .find_map(|chunk| chunk.strip_prefix("go"))
        .and_then(|version| version.get(..4))
        .and_then(|version| version.parse::<f32>().ok())
        .unwrap_or_else(|| panic!("failed to parse Go runtime version {version:?}"))
        .into()
}
