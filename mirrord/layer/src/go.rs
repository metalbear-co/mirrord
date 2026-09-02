//! mirrord-layer's dispatch table for syscalls made by a Go runtime.
//!
//! [`mirrord_layer_go`] owns the assembly trampolines that catch syscalls issued directly by the
//! Go runtime; this module supplies the table deciding which of them get routed into the layer's
//! detours, and which are passed through to the kernel unchanged.
#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]

use mirrord_layer_go::{Handlers, passthrough};
use nix::errno::Errno;
use tracing::trace;

use crate::{close_detour, file::hooks::*, hooks::HookManager, socket::hooks::*};

const HANDLERS: Handlers = Handlers {
    syscall: c_abi_syscall_handler,
    syscall6: c_abi_syscall6_handler,
};

/// Installs the Go syscall hooks, routing intercepted syscalls into the layer's detours.
pub(crate) fn enable_hooks(hook_manager: &mut HookManager, use_asmcgocall: bool) {
    mirrord_layer_go::init(HANDLERS);
    mirrord_layer_go::enable_hooks(hook_manager, use_asmcgocall);
}

/// Same as [`enable_hooks`], but hooks symbols found in the given `module_name`.
pub(crate) fn enable_hooks_in_loaded_module(
    hook_manager: &mut HookManager,
    module_name: String,
    use_asmcgocall: bool,
) {
    mirrord_layer_go::init(HANDLERS);
    mirrord_layer_go::enable_hooks_in_loaded_module(hook_manager, module_name, use_asmcgocall);
}

/// Syscall & Syscall6 handler - supports upto 6 params, mainly used for
/// accept4 Note: Depending on success/failure Syscall may or may not call this handler
unsafe extern "C" fn c_abi_syscall6_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    unsafe {
        mirrord_layer_macro::trace!(
            "c_abi_syscall6_handler: syscall={} param1={} param2={} param3={} param4={} param5={} param6={}",
            syscall,
            param1,
            param2,
            param3,
            param4,
            param5,
            param6
        );
        let syscall_result = match syscall {
            libc::SYS_accept4 => {
                accept4_detour(param1 as _, param2 as _, param3 as _, param4 as _) as i64
            }
            libc::SYS_socket => socket_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_bind => bind_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_listen => listen_detour(param1 as _, param2 as _) as i64,
            libc::SYS_accept => accept_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_close => close_detour(param1 as _) as i64,
            libc::SYS_connect => connect_detour(param1 as _, param2 as _, param3 as _) as i64,

            _ if crate::setup().fs_config().is_active() => {
                match syscall {
                    libc::SYS_read => read_detour(param1 as _, param2 as _, param3 as _) as i64,
                    libc::SYS_pread64 => {
                        pread_detour(param1 as _, param2 as _, param3 as _, param4 as _) as i64
                    }
                    libc::SYS_write => write_detour(param1 as _, param2 as _, param3 as _) as i64,
                    libc::SYS_pwrite64 => {
                        pwrite_detour(param1 as _, param2 as _, param3 as _, param4 as _) as i64
                    }
                    libc::SYS_lseek => lseek_detour(param1 as _, param2 as _, param3 as _),
                    // Note(syscall_linux.go)
                    // if flags == 0 {
                    // 	return faccessat(dirfd, path, mode)
                    // }
                    // The Linux kernel faccessat system call does not take any flags.
                    // The glibc faccessat implements the flags itself; see
                    // https://sourceware.org/git/?p=glibc.git;a=blob;f=sysdeps/unix/sysv/linux/faccessat.c;hb=HEAD
                    // Because people naturally expect syscall.Faccessat to act
                    // like C faccessat, we do the same.
                    libc::SYS_faccessat => {
                        faccessat_detour(param1 as _, param2 as _, param3 as _, 0) as i64
                    }
                    // Stat hooks:
                    // - SYS_stat: maps to fstatat with AT_FDCWD in go - no additional hook needed
                    // |-- fstatat(_AT_FDCWD, path, stat, 0)
                    // - SYS_fstat will use fstat_detour, maps to the same syscall number i.e.
                    //   SYS_FSTAT (5)
                    // - SYS_newfstatat will use fstatat_detour, maps to the same syscall number
                    //   i.e. SYS_NEWFSTATAT (262)
                    // - SYS_lstat: maps to fstatat with AT_FDCWD and AT_SYMLINK_NOFOLLOW in go - no
                    //   additional hook needed
                    // - SYS_statx: not supported in go
                    libc::SYS_newfstatat => {
                        fstatat_logic(param1 as _, param2 as _, param3 as _, param4 as _)
                            .unwrap_or_bypass_with(|_| {
                                passthrough(syscall, param1, param2, param3, param4, param5, param6)
                                    as i32
                            })
                            .into()
                    }
                    libc::SYS_fstat => fstat_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_statfs => statfs64_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_fstatfs => fstatfs64_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_fsync => fsync_detour(param1 as _) as i64,
                    libc::SYS_fdatasync => fsync_detour(param1 as _) as i64,
                    libc::SYS_openat => {
                        openat_detour(param1 as _, param2 as _, param3 as _, param4 as libc::c_int)
                            as i64
                    }
                    libc::SYS_getdents64 => {
                        getdents64_detour(param1 as _, param2 as _, param3 as _) as i64
                    }
                    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                    libc::SYS_rename => rename_detour(param1 as _, param2 as _) as i64,

                    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                    libc::SYS_mkdir => mkdir_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_mkdirat => {
                        mkdirat_detour(param1 as _, param2 as _, param3 as _) as i64
                    }
                    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                    libc::SYS_rmdir => rmdir_detour(param1 as _) as i64,
                    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                    libc::SYS_unlink => unlink_detour(param1 as _) as i64,
                    libc::SYS_unlinkat => {
                        unlinkat_detour(param1 as _, param2 as _, param3 as _) as i64
                    }
                    _ => passthrough(syscall, param1, param2, param3, param4, param5, param6),
                }
            }
            _ => passthrough(syscall, param1, param2, param3, param4, param5, param6),
        };

        if syscall_result.is_negative() {
            // Might not be an exact mapping, but it should be good enough.
            -(Errno::last_raw() as i64)
        } else {
            syscall_result
        }
    }
}

/// Syscall & Rawsyscall handler - supports upto 4 params, used for socket,
/// bind, listen, and accept
/// Note: Depending on success/failure Syscall may or may not call this handler
unsafe extern "C" fn c_abi_syscall_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
) -> i64 {
    unsafe {
        trace!(
            "c_abi_syscall_handler: syscall={} param1={} param2={} param3={}",
            syscall, param1, param2, param3
        );
        let syscall_result = match syscall {
            libc::SYS_socket => socket_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_bind => bind_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_listen => listen_detour(param1 as _, param2 as _) as i64,
            libc::SYS_connect => connect_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_accept => accept_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_close => close_detour(param1 as _) as i64,

            _ if crate::setup().fs_config().is_active() => match syscall {
                libc::SYS_read => read_detour(param1 as _, param2 as _, param3 as _) as i64,
                libc::SYS_write => write_detour(param1 as _, param2 as _, param3 as _) as i64,
                libc::SYS_lseek => lseek_detour(param1 as _, param2 as _, param3 as _),
                // Note(syscall_linux.go)
                // if flags == 0 {
                // 	return faccessat(dirfd, path, mode)
                // }
                // The Linux kernel faccessat system call does not take any flags.
                // The glibc faccessat implements the flags itself; see
                // https://sourceware.org/git/?p=glibc.git;a=blob;f=sysdeps/unix/sysv/linux/faccessat.c;hb=HEAD
                // Because people naturally expect syscall.Faccessat to act
                // like C faccessat, we do the same.
                libc::SYS_faccessat => {
                    faccessat_detour(param1 as _, param2 as _, param3 as _, 0) as i64
                }
                libc::SYS_fstat => fstat_detour(param1 as _, param2 as _) as i64,
                libc::SYS_statfs => statfs64_detour(param1 as _, param2 as _) as i64,
                libc::SYS_fstatfs => fstatfs64_detour(param1 as _, param2 as _) as i64,
                libc::SYS_getdents64 => {
                    getdents64_detour(param1 as _, param2 as _, param3 as _) as i64
                }
                #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                libc::SYS_rename => rename_detour(param1 as _, param2 as _) as i64,
                #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                libc::SYS_mkdir => mkdir_detour(param1 as _, param2 as _) as i64,
                libc::SYS_mkdirat => mkdirat_detour(param1 as _, param2 as _, param3 as _) as i64,
                #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                libc::SYS_rmdir => rmdir_detour(param1 as _) as i64,
                #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
                libc::SYS_unlink => unlink_detour(param1 as _) as i64,
                libc::SYS_unlinkat => unlinkat_detour(param1 as _, param2 as _, param3 as _) as i64,
                _ => passthrough(syscall, param1, param2, param3, 0, 0, 0),
            },
            _ => passthrough(syscall, param1, param2, param3, 0, 0, 0),
        };

        if syscall_result.is_negative() {
            // Might not be an exact mapping, but it should be good enough.
            -(Errno::last_raw() as i64)
        } else {
            syscall_result
        }
    }
}
