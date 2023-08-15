use std::arch::asm;

use errno::errno;
use tracing::trace;

use crate::{
    close_detour, file::hooks::*, hooks::HookManager, macros::hook_symbol, socket::hooks::*,
    FILE_MODE,
};



/// [Naked function] maps to gasave_systemstack_switch, called by asmcgocall.abi0
#[no_mangle]
#[naked]
unsafe extern "C" fn gosave_systemstack_switch() {
    asm!(
        "adrp       x0,0x73000",
        "add        x0,x0,0x3d0",
        "add        x0,x0,0x8",
        "str        x0,[x28, 0x40]",
        "mov        x0,sp",
        "str        x0,[x28, 0x38]",
        "str        x29,[x28, 0x68]",
        "str        xzr,[x28, 0x60]",
        "str        xzr,[x28, 0x58]",
        "ldr        x0,[x28, 0x50]",
        "cbz        x0,1f",
        "bl         go_runtime_abort",
        "1:",
        "ret",
        options(noreturn)
    );
}


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

        _ if FILE_MODE
            .get()
            .expect("FILE_MODE needs to be initialized")
            .is_active() =>
        {
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

/// [Naked function] 6 param version, used by Rawsyscall & Syscall
#[naked]
unsafe extern "C" fn syscall_6(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    asm!(
        "mov    x8, x0",
        "mov    x0, x1",
        "mov    x1, x2",
        "mov    x2, x3",
        "mov    x3, x4",
        "mov    x4, x5",
        "mov    x5, x6",
        "svc 0x0",
        "ret",
        options(noreturn)
    )
}

/// runtime.save_g.abi0
#[no_mangle]
#[naked]
unsafe extern "C" fn go_runtime_save_g() {
    asm!(
        // save_g implementation, minus cgo check
        "adrp x27, 0x660000",
        "mrs x0, tpidr_el0",
        "mov x27, 0x10",
        "str x28, [x0, x27, LSL 0x0]",
        "ret",
        options(noreturn)
    );
}

/// [Naked function] maps to runtime.abort.abi0, called by `gosave_systemstack_switch`
#[no_mangle]
#[naked]
unsafe extern "C" fn go_runtime_abort() {
    asm!("mov x0, xzr", "ldr x0, [x0]", options(noreturn));
}

/// Detour for Go >= 1.19
/// On Go 1.19 one hook catches all (?) syscalls and therefore we call the syscall6 handler always
/// so syscall6 handler need to handle syscall3 detours as well.
#[naked]
unsafe extern "C" fn go_syscall_new_detour() {
    asm!(
        // save args from stack into registers
        "ldr x9, [sp, 0x8]",
        "ldr x10, [sp, 0x10]",
        "ldr x11, [sp, 0x18]",
        "ldr x12, [sp, 0x20]",
        "ldr x13, [sp, 0x28]",
        "ldr x14, [sp, 0x30]",
        "ldr x15, [sp, 0x38]",
        // adjusted copy of `asmcgocall`
        // not sure where this prologue comes from but okay
        "str x30, [sp, -0x10]!",
        "stur x29, [sp, -0x8]",
        "sub x29, sp, 0x8",
        // save sp into x2
        "mov x2, sp",
        // no g, no save
        "cbz x28, 2f",
        "mov x4, x28",
        "ldr x8, [x28, 0x30]",
        "ldr x3, [x8, 0x50]",
        // check if g = gsignal
        "cmp x28, x3",
        "b.eq 2f",
        // check if g0 = g
        "ldr x3, [x8]",
        "cmp x28, x3",
        "b.eq 2f",
        "bl gosave_systemstack_switch",
        // save_g implementation, minus cgo check
        "mov x28, x3",
        "bl go_runtime_save_g",
        "ldr x0, [x28, 0x38]",
        "mov sp, x0",
        "ldr x29, [x28, 0x68]",
        // adjust stack? - this is like asmcgocall but using x3 instead of x13
        // to avoid losing the data
        "mov x3, sp",
        "sub x3, x3, 0x10",
        "mov sp, x3",
        "str x4, [sp]",
        "ldr x4, [x4, 0x8]",
        "sub x4,x4,x2",
        "str x4, [sp, 0x8]",
        // prepare arguments
        "mov x0, x9",
        "mov x1, x10",
        "mov x2, x11",
        "mov x3, x12",
        "mov x4, x13",
        "mov x5, x14",
        "mov x6, x15",
        "bl c_abi_syscall6_handler",
        "mov x9, x0",
        "ldr x28, [sp]",
        "bl go_runtime_save_g",
        "ldr x5, [x28, 0x8]",
        "ldr x6, [sp, 0x8]",
        "sub x5, x5, x6",
        "mov x0, x9",
        "mov sp, x5",
        "ldp x29, x30, [sp, -0x8]",
        "add sp, sp ,0x10",
        "b 3f",
        "2:", //noswitch
        // we can just use the stack
        // The function receives syscall, arg1, arg2, arg3, arg4, arg5, arg6 from stack
        // starting with SP+8
        "mov x3, sp",
        "sub x3, x3, 0x10",
        "mov sp, x3",
        "mov x4, xzr",
        "str x4, [sp]",
        "str x2, [sp, 0x8]",
        "bl c_abi_syscall6_handler",
        "ldr x2, [sp, 0x8]",
        "mov sp, x2",
        "ldp x29, x30, [sp, -0x8]",
        "add sp, sp ,0x10",
        // aftercall
        "3:",
        // check return code
        "cmn x0, 0xfff",
        // jump to success if return code == 0
        "b.cc 4f",
        // syscall fail flow
        "mov x4, -0x1",
        "str x4, [sp, 0x40]",
        "str xzr, [sp, 0x48]",
        "neg x0, x0",
        "str x0, [sp, 0x50]",
        "ret",
        // syscall success
        "4:",
        "str x0, [sp, 0x40]",
        "str x1, [sp, 0x48]",
        "str xzr, [sp, 0x50]",
        "ret",
        options(noreturn)
    )
}

// /// Detour for Go >= 1.19
// /// On Go 1.19 one hook catches all (?) syscalls and therefore we call the syscall6 handler always
// /// so syscall6 handler need to handle syscall3 detours as well.
// #[naked]
// unsafe extern "C" fn go_syscall_new_detour() {
//     asm!(
//         // adjusted copy of `runtime.systemstack.abi0`
//         // I'm not sure why it does this but okay
//         // x28 = g, load m from it into x4
//         "ldr x4, [x28, 0x30]",
//         // store gsignal in x5
//         "ldr x5, [x4, 0x50]",
//         // check if g = gsignal
//         "cmp x5, x28",
//         // if equal, jump to noswitch
//         "b.eq 2f",
//         // load g0 into r5, see if it's same as our g
//         // if it is, jump to noswitch
//         "ldr x5, [x4]",
//         "cmp x5, x28",
//         "b.eq 2f",
//         // if curg == g jmp to switch, if not, crash.
//         "ldr x6, [x4, 0xc0]",
//         "cmp x6, x28",
//         "b.eq 1f",
//         "b go_runtime_abort",
//         "1:", // switch
//         // save arguments to registers before replacing stack
//         "ldr x9, [sp, 0x8]",
//         "ldr x10, [sp, 0x10]",
//         "ldr x11, [sp, 0x18]",
//         "ldr x12, [sp, 0x20]",
//         "ldr x13, [sp, 0x28]",
//         "ldr x14, [sp, 0x30]",
//         "ldr x15, [sp, 0x38]",
//         "bl gosave_systemstack_switch",
//         // save_g implementation, minus cgo check
//         "mov x28, x5",
//         "bl go_runtime_save_g",
//         "ldr x3, [x28, 0x38]",
//         "mov sp, x3",
//         "ldr x29, [x28, 0x68]",
//         "mov x0, x9",
//         "mov x1, x10",
//         "mov x2, x11",
//         "mov x3, x12",
//         "mov x4, x13",
//         "mov x5, x14",
//         "mov x6, x15",
//         "bl c_abi_syscall6_handler",
//         "mov x15, x0",
//         "ldr x3, [x28, 0x30]",
//         "ldr x28, [x3, 0xc0]",
//         "bl go_runtime_save_g",
//         "ldr x0,[x28, 0x38]",
//         "mov sp, x0",
//         "ldr x29, [x28, 0x68]",
//         "str xzr, [x28, 0x38]",
//         "str xzr, [x28, 0x68]",
//         "mov x0, x15",
//         "b 3f",
//         "2:", //noswitch
//         // we can just use the stack
//         // The function receives syscall, arg1, arg2, arg3, arg4, arg5, arg6 from stack
//         // starting with SP+8
//         "ldr x0, [sp, 0x8]",
//         "ldr x1, [sp, 0x10]",
//         "ldr x2, [sp, 0x18]",
//         "ldr x3, [sp, 0x20]",
//         "ldr x4, [sp, 0x28]",
//         "ldr x5, [sp, 0x30]",
//         "ldr x6, [sp, 0x38]",
//         "bl c_abi_syscall6_handler",
//         // aftercall
//         "3:",
//         // check return code
//         "cmn x0, 0xfff",
//         // jump to success if return code == 0
//         "b.cc 4f",
//         // syscall fail flow
//         "mov x4, -0x1",
//         "str x4, [sp, 0x40]",
//         "str xzr, [sp, 0x48]",
//         "neg x0, x0",
//         "str x0, [sp, 0x50]",
//         "ret",
//         // syscall success
//         "4:",
//         "str x0, [sp, 0x40]",
//         "str x1, [sp, 0x48]",
//         "str xzr, [sp, 0x50]",
//         "ret",
//         options(noreturn)
//     )
// }


/// Hooks for when hooking a post go 1.19 binary
fn post_go1_19(hook_manager: &mut HookManager) {
    hook_symbol!(
        hook_manager,
        "runtime/internal/syscall.Syscall6.abi0",
        go_syscall_new_detour
    );
}

/// Note: We only hook "RawSyscall", "Syscall6", and "Syscall" because for our usecase,
/// when testing with "Gin", only these symbols were used to make syscalls.
/// Refer:
///   - File zsyscall_linux_amd64.go generated using mksyscall.pl.
///   - <https://cs.opensource.google/go/go/+/refs/tags/go1.18.5:src/syscall/syscall_unix.go>
pub(crate) fn enable_hooks(hook_manager: &mut HookManager) {
    if let Some(version_symbol) =
        hook_manager.resolve_symbol_main_module("runtime.buildVersion.str")
    {
        // Version str is `go1.xx` - take only last 4 characters.
        let version = unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                version_symbol.0.add(2) as *const u8,
                4,
            ))
        };
        let version_parsed: f32 = version.parse().unwrap();
        if version_parsed >= 1.19 {
            trace!("found version >= 1.19");
            post_go1_19(hook_manager);
        } else {
            trace!("found version < 1.19, arm64 not supported - not hooking");
        }
    }
}
