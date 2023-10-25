#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
use errno::errno;
use tracing::trace;

use crate::{close_detour, file::hooks::*, socket::hooks::*};

#[cfg_attr(
    all(target_os = "linux", target_arch = "x86_64"),
    path = "linux_x64.rs"
)]
#[cfg_attr(
    all(target_os = "linux", target_arch = "aarch64"),
    path = "linux_aarch64.rs"
)]
pub(crate) mod go_hooks;

use go_hooks::syscall_6;
/// Syscall & Syscall6 handler - supports upto 6 params, mainly used for
/// accept4 Note: Depending on success/failure Syscall may or may not call this handler
#[no_mangle]
unsafe extern "C" fn c_abi_syscall6_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    trace!(
        "c_abi_syscall6_handler: syscall={} param1={} param2={} param3={} param4={} param5={} param6={}",
        syscall, param1, param2, param3, param4, param5, param6
    );
    let res = match syscall {
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
                // Stat hooks:
                // - SYS_stat: maps to fstatat with AT_FDCWD in go - no additional hook needed
                // |-- fstatat(_AT_FDCWD, path, stat, 0)
                // - SYS_fstat will use fstat_detour, maps to the same syscall number i.e. SYS_FSTAT
                //   (5)
                // - SYS_newfstatat will use fstatat_detour, maps to the same syscall number i.e.
                //   SYS_NEWFSTATAT (262)
                // - SYS_lstat: maps to fstatat with AT_FDCWD and AT_SYMLINK_NOFOLLOW in go - no
                //   additional hook needed
                // - SYS_statx: not supported in go
                libc::SYS_newfstatat => {
                    fstatat_logic(param1 as _, param2 as _, param3 as _, param4 as _)
                        .unwrap_or_bypass_with(|_| {
                            syscall_6(syscall, param1, param2, param3, param4, param5, param6)
                                .try_into()
                                .unwrap()
                        })
                        .into()
                }
                libc::SYS_openat => openat_detour(param1 as _, param2 as _, param3 as _) as i64,
                libc::SYS_getdents64 => {
                    getdents64_detour(param1 as _, param2 as _, param3 as _) as i64
                }
                _ => syscall_6(syscall, param1, param2, param3, param4, param5, param6),
            }
        }
        _ => syscall_6(syscall, param1, param2, param3, param4, param5, param6),
    };
    match res {
        -1 => -errno().0 as i64,
        _ => res,
    }
}
