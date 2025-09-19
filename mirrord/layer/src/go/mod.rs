#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
use std::ffi::CStr;

use nix::errno::Errno;
use tracing::trace;

use crate::{close_detour, file::hooks::*, hooks::HookManager, socket::hooks::*};

#[cfg_attr(
    all(target_os = "linux", target_arch = "x86_64"),
    path = "linux_x64.rs"
)]
#[cfg_attr(
    all(target_os = "linux", target_arch = "aarch64"),
    path = "linux_aarch64.rs"
)]
pub(crate) mod go_hooks;

/// Syscall & Syscall6 handler - supports upto 6 params, mainly used for
/// accept4 Note: Depending on success/failure Syscall may or may not call this handler
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
    unsafe {
        trace!(
            "c_abi_syscall6_handler: syscall={} param1={} param2={} param3={} param4={} param5={} param6={}",
            syscall, param1, param2, param3, param4, param5, param6
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

            _ if crate::SETUP
                .get()
                .map_or_else(|| {
                    trace!("c_abi_syscall6_handler: SETUP not initialized yet for syscall {}", syscall);
                    false
                }, |setup| {
                    let is_active = setup.fs_config().is_active();
                    trace!("c_abi_syscall6_handler: SETUP initialized, fs_config().is_active() = {} for syscall {}", is_active, syscall);
                    is_active
                }) =>
            {
                match syscall {
                    libc::SYS_read => {
                        trace!("c_abi_syscall6_handler: handling SYS_read via mirrord");
                        read_detour(param1 as _, param2 as _, param3 as _) as i64
                    }
                    libc::SYS_pread64 => {
                        trace!("c_abi_syscall6_handler: handling SYS_pread64 via mirrord");
                        pread_detour(param1 as _, param2 as _, param3 as _, param4 as _) as i64
                    }
                    libc::SYS_write => {
                        trace!("c_abi_syscall6_handler: handling SYS_write via mirrord");
                        write_detour(param1 as _, param2 as _, param3 as _) as i64
                    }
                    libc::SYS_pwrite64 => {
                        trace!("c_abi_syscall6_handler: handling SYS_pwrite64 via mirrord");
                        pwrite_detour(param1 as _, param2 as _, param3 as _, param4 as _) as i64
                    }
                    libc::SYS_lseek => {
                        trace!("c_abi_syscall6_handler: handling SYS_lseek via mirrord");
                        lseek_detour(param1 as _, param2 as _, param3 as _)
                    }
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
                        trace!("c_abi_syscall6_handler: handling SYS_faccessat via mirrord");
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
                                let (Ok(result) | Err(result)) = syscalls::syscall!(
                                    syscalls::Sysno::from(syscall as i32),
                                    param1,
                                    param2,
                                    param3,
                                    param4,
                                    param5,
                                    param6
                                )
                                .map(|success| success as i64)
                                .map_err(|fail| {
                                    let raw_errno = fail.into_raw();
                                    Errno::set_raw(raw_errno);

                                    -(raw_errno as i64)
                                });
                                result as i32
                            })
                            .into()
                    }
                    libc::SYS_fstat => fstat_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_statfs => statfs64_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_fstatfs => fstatfs64_detour(param1 as _, param2 as _) as i64,
                    libc::SYS_fsync => fsync_detour(param1 as _) as i64,
                    libc::SYS_fdatasync => fsync_detour(param1 as _) as i64,
                    libc::SYS_openat => {
                        trace!("c_abi_syscall6_handler: handling SYS_openat via mirrord");
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
                    _ => {
                        trace!("c_abi_syscall6_handler: unknown fs syscall {} bypassing to kernel", syscall);
                        let (Ok(result) | Err(result)) = syscalls::syscall!(
                            syscalls::Sysno::from(syscall as i32),
                            param1,
                            param2,
                            param3,
                            param4,
                            param5,
                            param6
                        )
                        .map(|success| success as i64)
                        .map_err(|fail| {
                            let raw_errno = fail.into_raw();
                            Errno::set_raw(raw_errno);

                            -(raw_errno as i64)
                        });
                        result
                    }
                }
            }
            _ => {
                trace!("c_abi_syscall6_handler: syscall {} bypassing - either not fs-related or layer not ready", syscall);
                let (Ok(result) | Err(result)) = syscalls::syscall!(
                    syscalls::Sysno::from(syscall as i32),
                    param1,
                    param2,
                    param3,
                    param4,
                    param5,
                    param6
                )
                .map(|success| success as i64)
                .map_err(|fail| {
                    let raw_errno = fail.into_raw();
                    Errno::set_raw(raw_errno);

                    -(raw_errno as i64)
                });
                result
            }
        };

        if syscall_result.is_negative() {
            // Might not be an exact mapping, but it should be good enough.
            -(Errno::last_raw() as i64)
        } else {
            syscall_result
        }
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
    let version = unsafe {
        let cstr = CStr::from_ptr(version_symbol.0 as _);
        std::str::from_utf8_unchecked(cstr.to_bytes())
    };
    // buildVersion can look a bit complex:
    // devel go1.25-ecc06f0 Wed Apr 9 00:32:10 2025 -0700
    //
    // We need to find the word starting with 'go', and parse the next 4 characters.
    version
        .split_ascii_whitespace()
        .find_map(|chunk| chunk.strip_prefix("go"))
        .and_then(|version| version.get(..4))
        .and_then(|version| version.parse::<f32>().ok())
        .unwrap_or_else(|| panic!("failed to parse Go runtime version {version:?}"))
        .into()
}
