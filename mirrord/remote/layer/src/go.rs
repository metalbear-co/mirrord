//! remote-layer's dispatch table for syscalls made by a Go runtime.
//!
//! The libc hooks in [`crate::hooks`] never fire in a Go application: the Go runtime issues
//! syscalls directly rather than calling into libc, so an unhooked Go server would accept every
//! connection locally and the agent would never see any incoming traffic.
//!
//! [`mirrord_layer_go`] owns the assembly trampolines that catch those syscalls. This module
//! supplies the table routing them into the same detours the libc hooks use, so a Go application
//! goes through the exact same connection handoff and claimed-socket bookkeeping as any other.
//!
//! Only the accept and socket-address syscalls are routed. Everything else, including the
//! [`libc::SYS_close`] and `dup` family that maintain claimed-socket bookkeeping, is still passed
//! through unchanged, so a claimed placeholder fd that Go closes stays in
//! [`crate::claimed_sockets`] until its number is reused.
#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]

use mirrord_layer_core::hooks::HookManager;
use mirrord_layer_go::{Handlers, passthrough};
use nix::errno::Errno;

use crate::hooks::{accept_detour, accept4_detour, getpeername_detour, getsockname_detour};

/// Installs the Go syscall hooks, routing intercepted syscalls into the remote layer's detours.
///
/// A no-op when the process has no Go runtime in it.
pub(crate) fn enable_hooks(hook_manager: &mut HookManager) {
    mirrord_layer_go::init(Handlers {
        syscall: c_abi_syscall_handler,
        syscall6: c_abi_syscall6_handler,
    });

    // `runtime.asmcgocall` is the Go runtime's own way of running foreign C-ABI code on the `g0`
    // stack, and preserves the `g.sched` that `entersyscall` owns. The hand-rolled stack switch it
    // replaces can zero `g.sched.sp`/`bp` for goroutines that reached us mid-syscall.
    mirrord_layer_go::enable_hooks(hook_manager, true);
}

/// Entry point for the Go runtime's 6-argument syscall entry point.
unsafe extern "C" fn c_abi_syscall6_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    unsafe { dispatch(syscall, param1, param2, param3, param4, param5, param6) }
}

/// Entry point for the Go runtime's 3- and 4-argument syscall entry points.
///
/// Only Go runtimes older than 1.19 reach this; newer ones route everything through
/// [`c_abi_syscall6_handler`].
unsafe extern "C" fn c_abi_syscall_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
) -> i64 {
    unsafe { dispatch(syscall, param1, param2, param3, 0, 0, 0) }
}

/// Routes a syscall caught in the Go runtime to the matching detour.
///
/// Detours report failure the way libc does, by returning `-1` and setting `errno`, while the Go
/// runtime expects the raw kernel convention of `-errno`. Failures are translated back here.
unsafe fn dispatch(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    tracing::trace!(
        syscall,
        param1,
        param2,
        param3,
        param4,
        param5,
        param6,
        "remote-layer dispatching go syscall"
    );

    let result = unsafe {
        match syscall {
            libc::SYS_accept4 => {
                accept4_detour(param1 as _, param2 as _, param3 as _, param4 as _) as i64
            }
            libc::SYS_accept => accept_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_getsockname => {
                getsockname_detour(param1 as _, param2 as _, param3 as _) as i64
            }
            libc::SYS_getpeername => {
                getpeername_detour(param1 as _, param2 as _, param3 as _) as i64
            }
            _ => return passthrough(syscall, param1, param2, param3, param4, param5, param6),
        }
    };

    if result.is_negative() {
        // Might not be an exact mapping, but it should be good enough.
        -(Errno::last_raw() as i64)
    } else {
        result
    }
}
