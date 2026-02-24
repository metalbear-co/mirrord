use std::arch::naked_asm;

use nix::errno::Errno;
use tracing::trace;

use crate::{
    close_detour, file::hooks::*, hooks::HookManager, macros::hook_symbol, socket::hooks::*,
};
/*
 * Reference for which syscalls are managed by the handlers:
 * SYS_openat: Syscall6
 * SYS_read, SYS_write, SYS_lseek, SYS_faccessat: Syscall
 *
 * SYS_socket, SYS_bind, SYS_listen, SYS_accept, SYS_close: Syscall
 * SYS_accept4: Syscall6
 *
 * SYS_getdents64: Syscall on go 1.18, Syscall6 on go 1.19.
 */

/// [Naked function] This detour is taken from `runtime.asmcgocall.abi0`
/// Refer: <https://go.googlesource.com/go/+/refs/tags/go1.19rc2/src/runtime/asm_amd64.s#806>
/// Golang's assembler - <https://go.dev/doc/asm>
/// We cannot provide any stack guarantees when our detour executes(whether it will exceed the
/// go's stack limit), so we need to switch to system stack.
#[unsafe(naked)]
unsafe extern "C" fn go_rawsyscall_detour() {
    naked_asm!(
        // push the arguments of Rawsyscall from the stack to preserved registers
        "mov rbx, QWORD PTR [rsp+0x10]",
        "mov r10, QWORD PTR [rsp+0x18]",
        "mov rcx, QWORD PTR [rsp+0x20]",
        "mov rax, QWORD PTR [rsp+0x8]",
        // modified detour using asmcgocall.abi0 -
        "mov    rdx, rsp",
        // following instruction is the expansion of the get_tls() macro in go asm
        // TLS = Thread Local Storage
        // FS is a segment register used by go to save the current goroutine in TLS, in this
        // case `g`.
        "mov    rdi, QWORD PTR fs:[0xfffffff8]",
        "cmp    rdi, 0x0",
        "je     2f",
        "mov    r8, QWORD PTR [rdi+0x30]",
        "mov    rsi, QWORD PTR [r8+0x50]",
        "cmp    rdi, rsi",
        "je     2f",
        "mov    rsi, QWORD PTR [r8]",
        "cmp    rdi, rsi",
        "je     2f",
        "call   gosave_systemstack_switch",
        "mov    QWORD PTR fs:[0xfffffff8], rsi",
        "mov    rsp, QWORD PTR [rsi+0x38]",
        "sub    rsp, 0x40",
        "and    rsp, 0xfffffffffffffff0",
        "mov    QWORD PTR [rsp+0x30], rdi",
        "mov    rdi, QWORD PTR [rdi+0x8]",
        "sub    rdi, rdx",
        "mov    QWORD PTR [rsp+0x28],rdi",
        "mov    rsi, rbx",
        "mov    rdx, r10",
        "mov    rdi, rax",
        "call   c_abi_syscall_handler",
        "mov    rdi, QWORD PTR [rsp+0x30]",
        "mov    rsi, QWORD PTR [rdi+0x8]",
        "sub    rsi, QWORD PTR [rsp+0x28]",
        "mov    QWORD PTR fs:0xfffffff8, rdi",
        "mov    rsp, rsi",
        "cmp    rax, -0xfff",
        "jbe    3f",
        "mov    QWORD PTR [rsp+0x28], -0x1",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "neg    rax",
        "mov    QWORD PTR [rsp+0x38], rax",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        // same as `nosave` in the asmcgocall.
        // calls the abi handler, when we have no g
        "2:",
        "sub    rsp, 0x40",
        "and    rsp, -0x10",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "mov    QWORD PTR [rsp+0x28], rdx",
        "mov    rsi, rbx",
        "mov    rdx, r10",
        "mov    rdi, rax",
        "call   c_abi_syscall_handler",
        "mov    rsi, QWORD PTR [rsp+0x28]",
        "mov    rsp, rsi",
        "cmp    rax, -0xfff",
        "jbe    3f",
        // Success: Setup the return values and restore the tls.
        "mov    QWORD PTR [rsp+0x28], -0x1",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "neg    rax",
        "mov    QWORD PTR [rsp+0x38], rax",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        // Failure: Setup the return values and restore the tls.
        "3:",
        "mov    QWORD PTR [rsp+0x28], rax",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "mov    QWORD PTR [rsp+0x38], 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret"
    );
}

/// [Naked function] hook for Syscall6
#[unsafe(naked)]
unsafe extern "C" fn go_syscall6_detour() {
    naked_asm!(
        "mov rax, QWORD PTR [rsp+0x8]",
        "mov rbx, QWORD PTR [rsp+0x10]",
        "mov r10, QWORD PTR [rsp+0x18]",
        "mov rcx, QWORD PTR [rsp+0x20]",
        "mov r11, QWORD PTR [rsp+0x28]",
        "mov r12, QWORD PTR [rsp+0x30]",
        "mov r13, QWORD PTR [rsp+0x38]",
        "mov    rdx, rsp",
        "mov    rdi, QWORD PTR fs:[0xfffffff8]",
        "cmp    rdi, 0x0",
        "je     2f",
        "mov    r8, QWORD PTR [rdi+0x30]",
        "mov    rsi, QWORD PTR [r8+0x50]",
        "cmp    rdi,rsi",
        "je     2f",
        "mov    rsi, QWORD PTR [r8]",
        "cmp    rdi, rsi",
        "je     2f",
        "call   gosave_systemstack_switch",
        "mov    QWORD PTR fs:[0xfffffff8], rsi",
        "mov    rsp, QWORD PTR [rsi+0x38]",
        "sub    rsp, 0x40",
        "and    rsp, 0xfffffffffffffff0",
        "mov    QWORD PTR [rsp+0x30],rdi",
        "mov    rdi,QWORD PTR [rdi+0x8]",
        "sub    rdi,rdx",
        "mov    QWORD PTR [rsp+0x28],rdi",
        "mov    rsi, rbx",
        "mov    rdx, r10",
        "mov    rdi, rax",
        "mov    r8, r11",
        "mov    r9, r12",
        "mov    QWORD PTR [rsp], r13",
        "call   c_abi_syscall6_handler",
        "mov    rdi, QWORD PTR [rsp+0x30]",
        "mov    rsi, QWORD PTR [rdi+0x8]",
        "sub    rsi, QWORD PTR [rsp+0x28]",
        "mov    QWORD PTR fs:0xfffffff8, rdi",
        "mov    rsp, rsi",
        "cmp    rax, -0xfff",
        "jbe    3f",
        "mov    QWORD PTR [rsp+0x40], -0x1",
        "mov    QWORD PTR [rsp+0x48], 0x0",
        "neg    rax",
        "mov    QWORD PTR [rsp+0x50], rax",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        "2:",
        "sub    rsp, 0x40",
        "and    rsp, 0xfffffffffffffff0",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "mov    QWORD PTR [rsp+0x28], rdx",
        "mov    rsi, rbx",
        "mov    rdx, r10",
        "mov    rdi, rax",
        "mov    r8, r11",
        "mov    r9, r12",
        "mov     QWORD PTR [rsp], r13",
        "call   c_abi_syscall6_handler",
        "mov    rsi,QWORD PTR [rsp+0x28]",
        "mov    rsp, rsi",
        "cmp    rax, -0xfff",
        "jbe    3f",
        "mov    QWORD PTR [rsp+0x40], -0x1",
        "mov    QWORD PTR [rsp+0x48], 0x0",
        "neg    rax",
        "mov    QWORD PTR [rsp+0x50], rax",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        "3:",
        "mov    QWORD PTR [rsp+0x40], rax",
        "mov    QWORD PTR [rsp+0x48], 0x0",
        "mov    QWORD PTR [rsp+0x50], 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret"
    );
}

/// [Naked function] hook for Syscall
#[unsafe(naked)]
unsafe extern "C" fn go_syscall_detour() {
    naked_asm!(
        "mov rax, QWORD PTR [rsp+0x8]",
        "mov rbx, QWORD PTR [rsp+0x10]",
        "mov r10, QWORD PTR [rsp+0x18]",
        "mov rcx, QWORD PTR [rsp+0x20]",
        "mov    rdx, rsp",
        "mov    rdi, QWORD PTR fs:[0xfffffff8]",
        "cmp    rdi, 0x0",
        "je     2f",
        "mov    r8, QWORD PTR [rdi+0x30]",
        "mov    rsi, QWORD PTR [r8+0x50]",
        "cmp    rdi,rsi",
        "je     2f",
        "mov    rsi, QWORD PTR [r8]",
        "cmp    rdi, rsi",
        "je     2f",
        "call   gosave_systemstack_switch",
        "mov    QWORD PTR fs:[0xfffffff8], rsi",
        "mov    rsp, QWORD PTR [rsi+0x38]",
        "sub    rsp, 0x40",
        "and    rsp, 0xfffffffffffffff0",
        "mov    QWORD PTR [rsp+0x30],rdi",
        "mov    rdi, QWORD PTR [rdi+0x8]",
        "sub    rdi, rdx",
        "mov    QWORD PTR [rsp+0x28], rdi",
        "mov    rsi, rbx",
        "mov    rdx, r10",
        "mov    rdi, rax",
        "call   c_abi_syscall_handler",
        "mov    rdi, QWORD PTR [rsp+0x30]",
        "mov    rsi, QWORD PTR [rdi+0x8]",
        "sub    rsi, QWORD PTR [rsp+0x28]",
        "mov    QWORD PTR fs:0xfffffff8, rdi",
        "mov    rsp, rsi",
        "cmp    rax, -0xfff",
        "jbe    3f",
        "mov    QWORD PTR [rsp+0x28], -0x1",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "neg    rax",
        "mov    QWORD PTR [rsp+0x38], rax",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        "2:",
        "sub    rsp, 0x40",
        "and    rsp, 0xfffffffffffffff0",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "mov    QWORD PTR [rsp+0x28], rdx",
        "mov    rsi, rbx",
        "mov    rdx, r10",
        "mov    rdi, rax",
        "mov    r8, r11",
        "mov    r9, r12",
        "mov    QWORD PTR [rsp], r13",
        "call   c_abi_syscall6_handler",
        "mov    rsi, QWORD PTR [rsp+0x28]",
        "mov    rsp, rsi",
        "cmp    rax, -0xfff",
        "jbe    3f",
        "mov    QWORD PTR [rsp+0x28], -0x1",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "neg    rax",
        "mov    QWORD PTR [rsp+0x38], rax",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        "3:",
        "mov    QWORD PTR [rsp+0x28], rax",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "mov    QWORD PTR [rsp+0x38], 0x0",
        "xorps  xmm15,xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret"
    );
}

/// [Naked function] maps to gasave_systemstack_switch, called by asmcgocall.abi0
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn gosave_systemstack_switch() {
    naked_asm!(
        "lea    r9, [rip+0xdd9]",
        "mov    QWORD PTR [r14+0x40],r9",
        "lea    r9, [rsp+0x8]",
        "mov    QWORD PTR [r14+0x38],r9",
        "mov    QWORD PTR [r14+0x58],0x0",
        "mov    QWORD PTR [r14+0x68],rbp",
        "mov    r9, QWORD PTR [r14+0x50]",
        "test   r9, r9",
        "jz     4f",
        "call   go_runtime_abort",
        "4:",
        "ret"
    );
}

/// [Naked function] maps to runtime.abort.abi0, called by `gosave_systemstack_switch`
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn go_runtime_abort() {
    naked_asm!("int 0x3", "jmp go_runtime_abort");
}

/// Syscall & Rawsyscall handler - supports upto 4 params, used for socket,
/// bind, listen, and accept
/// Note: Depending on success/failure Syscall may or may not call this handler
#[unsafe(no_mangle)]
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
                _ => {
                    let (Ok(result) | Err(result)) = syscalls::syscall!(
                        syscalls::Sysno::from(syscall as i32),
                        param1,
                        param2,
                        param3
                    )
                    .map(|success| success as i64)
                    .map_err(|fail| {
                        let raw_errno = fail.into_raw();
                        Errno::set_raw(raw_errno);

                        -(raw_errno as i64)
                    });
                    result
                }
            },
            _ => {
                let (Ok(result) | Err(result)) = syscalls::syscall!(
                    syscalls::Sysno::from(syscall as i32),
                    param1,
                    param2,
                    param3
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

/// Detour for Go >= 1.19
/// On Go 1.19 one hook catches all (?) syscalls and therefore we call the syscall6 handler always
/// so syscall6 handler need to handle syscall3 detours as well.
#[unsafe(naked)]
unsafe extern "C" fn go_syscall_new_detour() {
    naked_asm!(
        "cmp rax, 60", // SYS_EXIT
        "je 4f",
        "cmp rax, 231", // SYS_EXIT_GROUP
        "je 4f",
        // Save rdi in r10
        "mov r10, rdi",
        // Save r9 in r11
        "mov r11, r9",
        // Save rax in r12
        "mov r12, rax",
        // save rsi in r13
        "mov r13, rsi",
        // save rcx in r15
        "mov r15, rcx",
        // Save stack
        "mov rdx, rsp",
        // in the past, we tried to retrieve g from fs:[-0x8]
        // but this sometimes fails to get g for some reason
        // and r14 seems to be more correct
        "mov rdi, r14",
        // for any case, store it there in case it isn't stored
        "mov qword ptr fs:[0xfffffff8], rdi",
        "cmp rdi, 0x0",
        "jz 2f",
        "mov rax, qword ptr [rdi + 0x30]",
        "mov rsi, qword ptr [rax + 0x50]",
        "cmp rdi, rsi",
        "jz 2f",
        "mov rsi, qword ptr [rax]",
        "cmp rdi, rsi",
        "jz 2f",
        "call gosave_systemstack_switch",
        "mov qword ptr FS:[0xfffffff8], rsi",
        "mov rsp, qword ptr [RSI + 0x38]",
        "sub rsp, 0x40",
        "and rsp, -0x10",
        "mov qword ptr [rsp + 0x30], rdi",
        "mov rdi, qword ptr [RDI + 0x8]",
        "sub RDI, RDX",
        "mov qword ptr [rsp + 0x28], rdi",
        // push the arguments of Rawsyscall from the stack to preserved registers
        "mov QWORD PTR [rsp], r11",
        "mov r9, r8",
        "mov r8, r13",
        "mov rsi, rbx",
        "mov rdx, r15",
        "mov rcx, r10",
        "mov rdi, r12",
        "call c_abi_syscall6_handler",
        // Switch stack back
        "mov rdi, qword ptr [rsp + 0x30]",
        "mov rsi, qword ptr [rdi + 0x8]",
        "sub rsi, qword ptr [ rsp + 0x28]",
        "mov qword ptr fs:[0xfffffff8], rdi",
        "mov rsp, rsi",
        // Regular flow
        "cmp    rax, -0xfff",
        "jbe    3f",
        "neg    rax",
        "mov    rcx, rax",
        "mov    rax, -0x1",
        "mov    rbx, 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        // same as `nosave` in the asmcgocall.
        // calls the abi handler, when we have no g
        "2:",
        "sub    rsp, 0x40",
        "and    rsp, -0x10",
        "mov    QWORD PTR [rsp+0x30], 0x0",
        "mov    QWORD PTR [rsp+0x28], rdx",
        // Call ABI handler
        "mov QWORD PTR [rsp], r11",
        "mov r9, r8",
        "mov r8, r13",
        "mov rsi, rbx",
        "mov rdx, r15",
        "mov rcx, r10",
        "mov rdi, r12",
        "call c_abi_syscall6_handler",
        // restore
        "mov    rsi, QWORD PTR [rsp+0x28]",
        "mov    rsp, rsi",
        // Regular flow
        "cmp    rax, -0xfff",
        "jbe    3f",
        "neg    rax",
        "mov    rcx, rax",
        "mov    rax, -0x1",
        "mov    rbx, 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        "3:",
        // RAX already contains return value
        "mov    rbx, 0x0",
        "mov    rcx, 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        // just execute syscall instruction
        // This is for SYS_EXIT and SYS_EXIT_GROUP only - we know for sure that it's safe to just
        // let it happen. Running our code is an unnecessary risk due to switching between
        // stacks. See issue https://github.com/metalbear-co/mirrord/issues/2988.
        "4:",
        "mov rdx, rdi",
        "syscall",
    )
}

/// Detour for `syscall.rawVforkSyscall.abi0` function.
#[unsafe(naked)]
unsafe extern "C" fn raw_vfork_detour() {
    naked_asm!(
        "mov rdi, qword ptr [rsp+0x10]",
        "mov rsi, qword ptr [rsp+0x18]",
        "mov rdx, qword ptr [rsp+0x20]",
        "mov rcx, qword ptr [rsp+0x8]",
        "pop r12",
        "call raw_vfork_handler",
        "push r12",
        "cmp rax, 0xfffffffffffff001",
        "jbe 2f",
        "mov qword ptr [rsp+0x28], -1",
        "neg rax",
        "mov qword ptr [rsp+0x30], rax",
        "ret",
        "2:",
        "mov qword ptr [rsp+0x28], rax",
        "mov qword ptr [rsp+0x30], 0",
        "ret",
    )
}

/// Hooks for when hooking a pre go 1.19 binary
fn pre_go1_19(hook_manager: &mut HookManager, module_name: Option<&str>) {
    if let Some(module_name) = module_name {
        hook_symbol!(
            hook_manager,
            module_name,
            "syscall.RawSyscall.abi0",
            go_rawsyscall_detour
        );
        hook_symbol!(
            hook_manager,
            module_name,
            "syscall.Syscall6.abi0",
            go_syscall6_detour
        );
        hook_symbol!(
            hook_manager,
            module_name,
            "syscall.Syscall.abi0",
            go_syscall_detour
        );
    } else {
        hook_symbol!(
            hook_manager,
            "syscall.RawSyscall.abi0",
            go_rawsyscall_detour
        );
        hook_symbol!(hook_manager, "syscall.Syscall6.abi0", go_syscall6_detour);
        hook_symbol!(hook_manager, "syscall.Syscall.abi0", go_syscall_detour);
    }
}

/// Hooks for when hooking a go binary between 1.19 and 1.23
fn post_go1_19(hook_manager: &mut HookManager, module_name: Option<&str>) {
    if let Some(module_name) = module_name {
        hook_symbol!(
            hook_manager,
            module_name,
            "runtime/internal/syscall.Syscall6",
            go_syscall_new_detour
        );
    } else {
        hook_symbol!(
            hook_manager,
            "runtime/internal/syscall.Syscall6",
            go_syscall_new_detour
        );
    }
}

/// Hooks for when hooking a post go 1.23 binary
fn post_go1_23(hook_manager: &mut HookManager, module_name: Option<&str>) {
    if let Some(module_name) = module_name {
        hook_symbol!(
            hook_manager,
            module_name,
            "internal/runtime/syscall.Syscall6",
            go_syscall_new_detour
        );
    } else {
        hook_symbol!(
            hook_manager,
            "internal/runtime/syscall.Syscall6",
            go_syscall_new_detour
        );
    }
}

/// Note: We only hook "RawSyscall", "Syscall6", and "Syscall" because for our usecase,
/// when testing with "Gin", only these symbols were used to make syscalls.
/// Refer:
///   - File zsyscall_linux_amd64.go generated using mksyscall.pl.
///   - <https://cs.opensource.google/go/go/+/refs/tags/go1.18.5:src/syscall/syscall_unix.go>
pub(crate) fn enable_hooks(hook_manager: &mut HookManager) {
    let Some(version) = super::get_go_runtime_version(hook_manager) else {
        return;
    };

    tracing::trace!(version, "Detected Go");
    if version >= 1.25 {
        post_go_1_25::hook(hook_manager, version);
    } else if version >= 1.23 {
        post_go1_23(hook_manager, None);
    } else if version >= 1.19 {
        post_go1_19(hook_manager, None);
    } else {
        pre_go1_19(hook_manager, None);
    }

    hook_symbol!(
        hook_manager,
        "syscall.rawVforkSyscall.abi0",
        raw_vfork_detour
    );
}

/// Same as [`enable_hooks`], but hook symbols found in the given `module_name`.
pub(crate) fn enable_hooks_in_loaded_module(hook_manager: &mut HookManager, module_name: String) {
    let Some(version) = super::get_go_runtime_version_in_module(hook_manager, &module_name) else {
        return;
    };

    if version >= 1.25 {
        post_go_1_25::hook_in_module(hook_manager, version, module_name.as_str());
    } else if version >= 1.23 {
        post_go1_23(hook_manager, Some(module_name.as_str()));
    } else if version >= 1.19 {
        post_go1_19(hook_manager, Some(module_name.as_str()));
    } else {
        pre_go1_19(hook_manager, Some(module_name.as_str()));
    }

    hook_symbol!(
        hook_manager,
        module_name.as_str(),
        "syscall.rawVforkSyscall.abi0",
        raw_vfork_detour
    );
}

/// Implementation for Go runtimes >= 1.25.
mod post_go_1_25 {
    use std::{arch::naked_asm, ffi::c_void};

    use crate::{hooks::HookManager, macros::hook_symbol};

    static mut FN_GOSAVE_SYSTEMSTACK_SWITCH: *const c_void = std::ptr::null();

    pub fn hook(hook_manager: &mut HookManager, go_version: f32) {
        let gosave_address = hook_manager
            .resolve_symbol_main_module("gosave_systemstack_switch")
            .expect(
                "couldn't find the address of symbol \
                `gosave_systemstack_switch`, please file a bug",
            );

        unsafe {
            FN_GOSAVE_SYSTEMSTACK_SWITCH = gosave_address.0;
        }

        let original_symbol = if go_version >= 1.26 {
            "internal/runtime/syscall/linux.Syscall6"
        } else {
            "internal/runtime/syscall.Syscall6"
        };

        hook_symbol!(
            hook_manager,
            original_symbol,
            internal_runtime_syscall_syscall6_detour
        );
    }

    pub fn hook_in_module(hook_manager: &mut HookManager, go_version: f32, module_name: &str) {
        let gosave_address = hook_manager
            .resolve_symbol_in_module(module_name, "gosave_systemstack_switch")
            .expect(
                "couldn't find the address of symbol \
                `gosave_systemstack_switch`, please file a bug",
            );

        unsafe {
            FN_GOSAVE_SYSTEMSTACK_SWITCH = gosave_address.0;
        }

        let original_symbol = if go_version >= 1.26 {
            "internal/runtime/syscall/linux.Syscall6"
        } else {
            "internal/runtime/syscall.Syscall6"
        };

        hook_symbol!(
            hook_manager,
            module_name,
            original_symbol,
            internal_runtime_syscall_syscall6_detour
        );
    }

    /// Detour for `internal/runtime/syscall.Syscall6`.
    ///
    /// Logic:
    /// 1. If the syscall is `SYS_EXIT` or `SYS_EXIT_GROUP`, let it happen.
    /// 2. Store syscall arguments on the stack.
    /// 3. Call [`c_abi_wrapper_on_systemstack`].
    /// 4. Put the result into correct registers, as expected by the caller.
    ///
    /// Essentially, a thin compatibility layer between the Go runtime code and the rest of our
    /// assembly glue.
    ///
    /// # Params
    ///
    /// `internal/runtime/syscall.Syscall6` params in rax, rbx, rcx, rdi, rsi, r8, r9.
    ///
    /// # Returns
    ///
    /// Mocked `internal/runtime/syscall.Syscall6` return values in rax, rbx, and rcx.
    ///
    /// # `SYS_EXIT` and `SYS_EXIT_GROUP`
    ///
    /// These two syscalls we should always pass through.
    /// See the [relevant issue](https://github.com/metalbear-co/mirrord/issues/2988)
    /// and the [fix](https://github.com/metalbear-co/mirrord/pull/2989).
    #[unsafe(naked)]
    unsafe extern "C" fn internal_runtime_syscall_syscall6_detour() {
        naked_asm!(
            // If the syscall is SYS_EXIT or SYS_EXIT_GROUP,
            // skip our logic and just execute it.
            "cmp    rax, 60",
            "je     2f",
            "cmp    rax, 231",
            "je     2f",
            // Save rbp.
            "push   rbp",
            "mov    rbp,rsp",
            // Reserve space for all arguments.
            "sub    rsp,0x40",
            // Move arguments from registers to the stack:
            // * syscall number is in rax
            // * 6 syscall arguments are in rbx, rcx, rdi, rsi, r8, r9
            //
            // Note that these are not the correct registers for making a syscall on x64.
            // These are the registers where arguments are stored
            // when `internal/runtime/syscall.Syscall6` is called.
            //
            // Note that we only move the arguments to the stack for simplicity.
            // We don't have to worry about overwriting the registers later.
            "mov    [rsp+0x38],r9",
            "mov    [rsp+0x30],r8",
            "mov    [rsp+0x28],rsi",
            "mov    [rsp+0x20],rdi",
            "mov    [rsp+0x18],rcx",
            "mov    [rsp+0x10],rbx",
            "mov    [rsp],rax",
            // Call `c_abi_wrapper_on_systemstack`, passing address of args in rdi.
            // Expect syscall result in rax.
            "mov    rdi,rsp",
            "call   {c_abi_wrapper_on_systemstack}",
            // Check failure.
            "cmp    rax,-0xfff",
            "jbe    1f",
            // Syscall failed.
            // Fill result registers: -1 in rax, errno in rcx, 0 in rbx.
            "neg    rax",
            "mov    rcx,rax",
            "mov    rax,-0x1",
            "mov    rbx,0x0",
            // Drop space reserved for locals.
            "add    rsp,0x40",
            // Restore rbp and return.
            "pop    rbp",
            "ret",
            // Syscall did not fail.
            // Result already in rax.
            // Clear the other two result registers.
            "1:",
            "mov    rbx,0x0",
            "mov    rcx,0x0",
            // Drop space reserved for locals.
            "add    rsp,0x40",
            // Restore rbp and return.
            "pop    rbp",
            "ret",
            // Move the first syscall argument to the correct register (rdx),
            // and execute the syscall.
            "2:",
            "mov    rdx, rdi",
            "syscall",

            c_abi_wrapper_on_systemstack = sym c_abi_wrapper_on_systemstack,
        )
    }

    /// Calls [`c_abi_wrapper`] on systemstack.
    ///
    /// Implemented based on <https://github.com/golang/go/blob/ef05b66d6115209361dd99ff8f3ab978695fd74a/src/runtime/asm_amd64.s#L481>.
    ///
    /// # Params
    ///
    /// * rdi - address of syscall number and arguments in memory.
    ///
    /// # Returns
    ///
    /// * rax - value returned by [`c_abi_wrapper`]
    #[unsafe(naked)]
    unsafe extern "C" fn c_abi_wrapper_on_systemstack() {
        naked_asm!(
            // Save rbp.
            // This is required, as we call `gosave_systemstack_switch` later.
            // Not having any local variables in this function is required as well.
            "push   rbp",
            "mov    rbp,rsp",
            // Load address of current m to rbx.
            // We assume that r14 stores the address of g.
            //
            // https://github.com/golang/go/blob/ef05b66d6115209361dd99ff8f3ab978695fd74a/src/runtime/runtime2.go#L408
            "mov    rbx,[r14+0x30]",
            // Check if g is m->gsignal.
            // If g is m->gsignal, no stack switch is required.
            //
            // https://github.com/golang/go/blob/ef05b66d6115209361dd99ff8f3ab978695fd74a/src/runtime/runtime2.go#L543
            "cmp    r14,[rbx+0x48]",
            "je     1f",
            // Load address of m->g0 to rdx.
            // Check if g is m->g0.
            // If g is m->g0, no stack switch is required.
            //
            // https://github.com/golang/go/blob/ef05b66d6115209361dd99ff8f3ab978695fd74a/src/runtime/runtime2.go#L537
            "mov    rdx,[rbx]",
            "cmp    r14,rdx",
            "je     1f",
            // Check if g is m->curg.
            // If current g is not m->curg, something weird is happening.
            // Jump to abort.
            "cmp    r14,[rbx+0xb8]",
            "jne    2f",
            // Switch stacks.
            //
            // This part I do not fully understand, but it works.
            //
            // Call to `gosave_systemstack_switch` allows us
            // to pretend we're systemstack_switch if the G stack is scanned,
            // whatever it means.
            //
            // After the call, rdx still contains address of m->g0.
            // We move it to TLS and r14 (runtime code sometimes expects it in r14),
            // and switch stacks by explicitly changing rsp.
            "mov    rsi,[rip + {gosave_systemstack_switch}]",
            "call   rsi",
            "mov    fs:0xfffffffffffffff8,rdx",
            "mov    r14,rdx",
            "mov    rsp,[rdx+0x38]",
            // At this point, rdi should still store the address of original syscall args in memory
            // (should not be smashed by `gosave_systemstack_switch`).
            // Original implementation relies on preserved rdi as well.
            // Call `c_abi_wrapper`.
            "call   {c_abi_wrapper}",
            // Switch back to g.
            // r14 should not have been changed inside `c_abi_wrapper`.
            "mov    rbx,[r14+0x30]",
            "mov    rsi,[rbx+0xb8]",
            "mov    fs:0xfffffffffffffff8,rsi",
            "mov    r14,rsi",
            "mov    rsp,[rsi+0x38]",
            "mov    rbp,[rsi+0x60]",
            "mov    QWORD PTR [rsi+0x38],0x0",
            "mov    QWORD PTR [rsi+0x60],0x0",
            // Restore rbp and return.
            "pop    rbp",
            "ret",
            // Already on system stack.
            "1:",
            // We're going to tail call `c_abi_wrapper`, so we need to pop rbp first.
            //
            // Quote the original implementation:
            // "Using a tail call here cleans up tracebacks since we won't stop at an intermediate systemstack"
            "pop    rbp",
            // At this point, rdi still stores the address of original syscall args in memory.
            // Call `c_abi_wrapper`.
            "jmp    {c_abi_wrapper}",
            // Current g is not gsignal, g0, nor curg.
            // Not idea what it is.
            // Original implementation calls an internal panic routine.
            // For simplicity, we explicitly trigger an invalid instruction interrupt.
            "2:",
            "ud2",

            gosave_systemstack_switch = sym FN_GOSAVE_SYSTEMSTACK_SWITCH,
            c_abi_wrapper = sym c_abi_wrapper,
        );
    }

    /// Calls [`c_abi_syscall6_handler`](crate::go::c_abi_syscall6_handler).
    ///
    /// Logic:
    /// 1. Make sure that stack conforms to C ABI.
    /// 2. Move call params from memory to correct registers/stack slots.
    ///
    /// Essentially, a thin compatibility layer between our assembly glue and C ABI.
    ///
    /// # Params
    ///
    /// * rdi - address of syscall number and arguments in memory.
    ///
    /// # Returns
    ///
    /// * rax - value returned by [`c_abi_syscall6_handler`](crate::go::c_abi_syscall6_handler).
    #[unsafe(naked)]
    unsafe extern "C" fn c_abi_wrapper() {
        naked_asm!(
            // Save rbp.
            "push   rbp",
            "mov    rbp,rsp",
            // Reserve space for the last `c_abi_syscall6_handler` param.
            "sub    rsp,0x8",
            // Align stack to 16-byte boundary.
            // This is required by C ABI.
            // This operation clears last 8 bytes of rsp.
            // This is either substracts 8 bytes of rsp or is a noop.
            // Therefore, in order to make space for the last `c_abi_syscall6_handler` param,
            // we still had to substract those 8 bytes before.
            "and    rsp,-0x10",
            // Prepare params and call `c_abi_syscall6_handler`.
            "mov    rax,[rdi]",      // syscall number -> tmp rax
            "mov    rsi,[rdi+0x10]", // arg1 -> rsi (C ABI param2)
            "mov    rdx,[rdi+0x18]", // arg2 -> rdx (C ABI param3)
            "mov    rcx,[rdi+0x20]", // arg3 -> rcx (C ABI param4)
            "mov    r8,[rdi+0x28]",  // arg4 -> r8  (C ABI param5)
            "mov    r9,[rdi+0x30]",  // arg5 -> r9  (C ABI param6)
            "mov    r10,[rdi+0x38]", // arg6 -> tmp r10
            "mov    [rsp],r10",      // arg6 -> stack (C ABI param7)
            "mov    rdi,rax",        // syscall number -> rdi (C ABI param1)
            "call   c_abi_syscall6_handler",
            // Restore rsp.
            "mov    rsp, rbp",
            // Restore rbp and return.
            // `c_abi_syscall6_handler` return value is already in rax.
            "pop    rbp",
            "ret"
        )
    }
}
