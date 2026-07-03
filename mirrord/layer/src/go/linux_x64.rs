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
pub(crate) fn enable_hooks(
    hook_manager: &mut HookManager,
    cgo_stack_switch: bool,
    use_asmcgocall: bool,
) {
    let Some(version) = super::get_go_runtime_version(hook_manager) else {
        return;
    };

    tracing::trace!(version, "Detected Go");
    if version >= 1.25 {
        post_go_1_25::hook(hook_manager, version, cgo_stack_switch, use_asmcgocall);
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
pub(crate) fn enable_hooks_in_loaded_module(
    hook_manager: &mut HookManager,
    module_name: String,
    cgo_stack_switch: bool,
    use_asmcgocall: bool,
) {
    let Some(version) = super::get_go_runtime_version_in_module(hook_manager, &module_name) else {
        return;
    };

    if version >= 1.25 {
        post_go_1_25::hook_in_module(
            hook_manager,
            version,
            module_name.as_str(),
            cgo_stack_switch,
            use_asmcgocall,
        );
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

    use crate::{go::c_abi_syscall6_handler, hooks::HookManager, macros::hook_symbol};

    static mut FN_GOSAVE_SYSTEMSTACK_SWITCH: *const c_void = std::ptr::null();

    /// Address of the Go runtime's `runtime.asmcgocall`, resolved at hook time and used by
    /// [`asmcgocall_syscall6_detour`].
    static mut FN_ASMCGOCALL: *const c_void = std::ptr::null();

    /// Offset, relative to the FS base, of the thread-local slot in which the Go runtime keeps the
    /// current goroutine pointer (`g`).
    ///
    /// The offset depends on how the binary was linked. Internally-linked binaries keep `g` at the
    /// classic `FS-8`, but externally-linked binaries — cgo with a large C thread-local footprint,
    /// e.g. statically linked OpenSSL/librdkafka — place it much further down (`FS-0x3ce0` was
    /// observed in the wild). Writing `g` to a hardcoded `FS-8` on such a binary lands in a dead
    /// slot, leaving the runtime's real `g` pointing at the user goroutine while we execute on the
    /// `g0` stack.
    ///
    /// Only the experimental `go_cgo_stack_switch` detour reads this. It defaults to `-8` so that,
    /// if resolution fails, behavior matches the previous hardcoded value.
    static mut G_TLS_OFFSET: i64 = -8;

    /// Find the `mov <reg>, fs:[disp32]` that loads Go's `g` from TLS in `code`, and return the
    /// signed `disp32` — the FS-relative offset of `g`.
    ///
    /// That load is always the same 9-byte instruction shape, so we slide a 9-byte window over
    /// `code` and match each field. Only an `fs:[disp32]` mov produces this exact 5-byte prefix,
    /// so ordinary stack spills (`mov [rsp+..], reg`: no `0x64`, SIB base `rsp` = `0x24`) never
    /// match. The trailing 4 bytes are the offset we want.
    ///
    /// ```text
    ///   64   48   8b   3c   25   20 c3 ff ff
    ///   │    │    │    │    │    └────┬─────┘  disp32 (little-endian) = 0xffffc320
    ///   │    │    │    │    │         └─ read as i32 -> -0x3ce0  (the offset of g)
    ///   │    │    │    │    └─ SIB: base = bare disp32, no index (absolute segment offset)
    ///   │    │    │    └─ ModRM: mod=00 rm=100 (a SIB byte follows); reg field ignored (& 0xc7)
    ///   │    │    └─ opcode: MOV r64<->r/m64  (0x8b load / 0x89 store)
    ///   │    └─ REX.W: 64-bit operand; low 3 bits vary with the dest register (& 0xf8 == 0x48)
    ///   └─ FS segment override: the access is relative to the FS base, where Go keeps g
    /// ```
    #[allow(clippy::indexing_slicing)]
    fn find_g_tls_offset(code: &[u8]) -> Option<i64> {
        code.windows(9).find_map(|window| {
            (window[0] == 0x64
                && (window[1] & 0xf8) == 0x48
                && (window[2] == 0x8b || window[2] == 0x89)
                && (window[3] & 0xc7) == 0x04
                && window[4] == 0x25)
                .then(|| i32::from_le_bytes([window[5], window[6], window[7], window[8]]) as i64)
        })
    }

    /// Resolve the FS-relative offset of the runtime's `g` thread-local from `runtime.asmcgocall`'s
    /// prologue (which loads `g` via `mov <reg>, fs:[disp32]`) and record it in [`G_TLS_OFFSET`].
    /// If the pattern isn't found the default (`-8`) is kept.
    fn resolve_g_tls_offset(asmcgocall_abi0: *const c_void) {
        // The `g` load lives in the prologue; 128 bytes is more than enough to reach it while
        // staying inside the runtime's `.text`.
        let bytes = unsafe { std::slice::from_raw_parts(asmcgocall_abi0 as *const u8, 128) };
        if let Some(offset) = find_g_tls_offset(bytes) {
            unsafe {
                G_TLS_OFFSET = offset;
            }
        }
    }

    pub fn hook(
        hook_manager: &mut HookManager,
        go_version: f32,
        cgo_stack_switch: bool,
        use_asmcgocall: bool,
    ) {
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

        if use_asmcgocall {
            let asmcgocall_address = hook_manager
                .resolve_symbol_main_module("runtime.asmcgocall")
                .expect(
                    "couldn't find the address of symbol \
                    `runtime.asmcgocall`, please file a bug",
                );
            unsafe {
                FN_ASMCGOCALL = asmcgocall_address.0;
            }
            hook_symbol!(hook_manager, original_symbol, asmcgocall_syscall6_detour);
        } else if cgo_stack_switch {
            if let Some(asmcgocall_abi0) =
                hook_manager.resolve_symbol_main_module("runtime.asmcgocall.abi0")
            {
                resolve_g_tls_offset(asmcgocall_abi0.0);
            }
            hook_symbol!(
                hook_manager,
                original_symbol,
                experimental_internal_runtime_syscall_syscall6_detour
            );
        } else {
            hook_symbol!(
                hook_manager,
                original_symbol,
                internal_runtime_syscall_syscall6_detour
            );
        }
    }

    pub fn hook_in_module(
        hook_manager: &mut HookManager,
        go_version: f32,
        module_name: &str,
        cgo_stack_switch: bool,
        use_asmcgocall: bool,
    ) {
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

        if use_asmcgocall {
            let asmcgocall_address = hook_manager
                .resolve_symbol_in_module(module_name, "runtime.asmcgocall")
                .expect(
                    "couldn't find the address of symbol \
                    `runtime.asmcgocall`, please file a bug",
                );
            unsafe {
                FN_ASMCGOCALL = asmcgocall_address.0;
            }
            hook_symbol!(
                hook_manager,
                module_name,
                original_symbol,
                asmcgocall_syscall6_detour
            );
        } else if cgo_stack_switch {
            if let Some(asmcgocall_abi0) =
                hook_manager.resolve_symbol_in_module(module_name, "runtime.asmcgocall.abi0")
            {
                resolve_g_tls_offset(asmcgocall_abi0.0);
            }
            hook_symbol!(
                hook_manager,
                module_name,
                original_symbol,
                experimental_internal_runtime_syscall_syscall6_detour
            );
        } else {
            hook_symbol!(
                hook_manager,
                module_name,
                original_symbol,
                internal_runtime_syscall_syscall6_detour
            );
        }
    }

    /// Layout of the syscall number and its arguments as spilled onto the goroutine stack by
    /// [`asmcgocall_syscall6_detour`] and read back by [`mirrord_syscall_handler`].
    #[repr(C)]
    struct SyscallArgs {
        syscall: i64,
        arg1: i64,
        arg2: i64,
        arg3: i64,
        arg4: i64,
        arg5: i64,
        arg6: i64,
    }

    /// The C-ABI function handed to `runtime.asmcgocall` to run on the `g0` stack.
    ///
    /// `asmcgocall` can only pass a single pointer argument, so [`asmcgocall_syscall6_detour`]
    /// spills the syscall number and arguments into a [`SyscallArgs`] on the goroutine stack and
    /// passes its address here.
    unsafe extern "C" fn mirrord_syscall_handler(args: *const SyscallArgs) -> i64 {
        unsafe {
            c_abi_syscall6_handler(
                (*args).syscall,
                (*args).arg1,
                (*args).arg2,
                (*args).arg3,
                (*args).arg4,
                (*args).arg5,
                (*args).arg6,
            )
        }
    }

    /// Detour for `internal/runtime/syscall.Syscall6` that reuses the Go runtime's own
    /// `runtime.asmcgocall` to switch to and from the `g0` system stack, instead of the hand-rolled
    /// switch in [`internal_runtime_syscall_syscall6_detour`] /
    /// [`experimental_c_abi_wrapper_on_systemstack`].
    ///
    /// This is what the arm64 hook has always done. `runtime.asmcgocall` is the runtime's
    /// sanctioned way to run foreign C-ABI code on `g0`, and is exactly what `cgocall` uses while
    /// the goroutine is `_Gsyscall`, so it preserves the `g.sched` that `entersyscall` owns for
    /// goroutines that reached us through `syscall.Syscall6`. It never zeroes `g.sched.sp`/`bp` —
    /// the corruption the hand-rolled [`internal_runtime_syscall_syscall6_detour`] path can cause.
    ///
    /// `asmcgocall` reads `g` from TLS and never writes `r14`, so the goroutine pointer the Go
    /// caller expects in `r14` survives the call; we still reload it from TLS before returning, to
    /// match the other detours. The full 64-bit handler result is left in `rax` by `asmcgocall`
    /// (its `MOVL` to the Go return slot only truncates the copy written to the stack).
    ///
    /// # Params
    ///
    /// `internal/runtime/syscall.Syscall6` params in rax, rbx, rcx, rdi, rsi, r8, r9.
    ///
    /// # Returns
    ///
    /// Mocked `internal/runtime/syscall.Syscall6` return values in rax, rbx, and rcx.
    #[unsafe(naked)]
    unsafe extern "C" fn asmcgocall_syscall6_detour() {
        naked_asm!(
            // SYS_EXIT and SYS_EXIT_GROUP must pass through untouched, exactly like the other
            // detours. See https://github.com/metalbear-co/mirrord/issues/2988.
            "cmp    rax, 60",
            "je     2f",
            "cmp    rax, 231",
            "je     2f",
            // Establish a frame and reserve space for the `asmcgocall` call args plus the
            // `SyscallArgs` struct (all relative to rsp after the `sub`):
            //   [rsp+0x00] fn        (asmcgocall arg0)
            //   [rsp+0x08] &args     (asmcgocall arg1)
            //   [rsp+0x10] ret slot  (asmcgocall int32 return, unused - we read rax directly)
            //   [rsp+0x18] args.syscall
            //   [rsp+0x20] args.arg1
            //   [rsp+0x28] args.arg2
            //   [rsp+0x30] args.arg3
            //   [rsp+0x38] args.arg4
            //   [rsp+0x40] args.arg5
            //   [rsp+0x48] args.arg6
            "push   rbp",
            "mov    rbp, rsp",
            "sub    rsp, 0x50",
            // Spill the syscall number and args from the Go ABIInternal registers to the struct.
            "mov    [rsp+0x18], rax",
            "mov    [rsp+0x20], rbx",
            "mov    [rsp+0x28], rcx",
            "mov    [rsp+0x30], rdi",
            "mov    [rsp+0x38], rsi",
            "mov    [rsp+0x40], r8",
            "mov    [rsp+0x48], r9",
            // asmcgocall(fn = mirrord_syscall_handler, arg = &args).
            "lea    rax, [rsp+0x18]",
            "mov    [rsp+0x08], rax",
            "lea    rax, [rip + {syscall_handler}]",
            "mov    [rsp], rax",
            "mov    rax, [rip + {asmcgocall}]",
            "call   rax",
            // Full 64-bit result is in rax. Tear down our frame.
            "add    rsp, 0x50",
            "pop    rbp",
            // Reload g into r14 for the Go caller's ABIInternal expectations.
            "mov    r14, QWORD PTR fs:[0xfffffff8]",
            // Format results as `internal/runtime/syscall.Syscall6` expects: r1=rax, r2=rbx,
            // errno=rcx, using the same error boundary as Go's own Syscall6.
            "cmp    rax, -0xfff",
            "jbe    3f",
            // Failure: errno in rcx, r1 = -1, r2 = 0.
            "neg    rax",
            "mov    rcx, rax",
            "mov    rax, -0x1",
            "mov    rbx, 0x0",
            "ret",
            // Success: r1 already in rax, clear r2 and errno.
            "3:",
            "mov    rbx, 0x0",
            "mov    rcx, 0x0",
            "ret",
            // SYS_EXIT / SYS_EXIT_GROUP passthrough.
            "2:",
            "mov    rdx, rdi",
            "syscall",

            syscall_handler = sym mirrord_syscall_handler,
            asmcgocall = sym FN_ASMCGOCALL,
        )
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

    /// Same as [`internal_runtime_syscall_syscall6_detour`] except this function calls
    /// [`experimental_c_abi_wrapper_on_systemstack`] which mimics how `cgocall` swtiches
    /// stack back to user g from g0.
    #[unsafe(naked)]
    unsafe extern "C" fn experimental_internal_runtime_syscall_syscall6_detour() {
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

            c_abi_wrapper_on_systemstack = sym experimental_c_abi_wrapper_on_systemstack,
        )
    }

    /// Calls [`c_abi_wrapper`] on systemstack.
    ///
    /// We hook `internal/runtime/syscall.Syscall6`, the leaf that issues the raw `SYSCALL`
    /// instruction. It is reached two ways:
    ///   - via `syscall.Syscall6`, which wraps the call in `runtime.entersyscall` /
    ///     `runtime.exitsyscall` — so the goroutine is **`_Gsyscall`** when we run, and
    ///     `entersyscall`'s `save()` already owns `g.sched`
    ///     (<https://github.com/golang/go/blob/go1.26.4/src/runtime/proc.go#L4806>);
    ///   - via `syscall.RawSyscall6`, with no wrapping — so the goroutine is `_Grunning`.
    ///
    /// `runtime.systemstack` is only valid on a `_Grunning` goroutine: its epilogue restores
    /// `rsp`/`rbp` from `g.sched` and then **zeroes `g.sched.sp`/`g.sched.bp`**
    /// (<https://github.com/golang/go/blob/go1.26.4/src/runtime/asm_amd64.s#L564-L573>).
    /// Doing that on a `_Gsyscall` goroutine corrupts the `g.sched` that `entersyscall` owns and
    /// leaves `g.sched.sp == 0` in the window before `exitsyscall` runs, which the scheduler/GC
    /// can observe.
    ///
    /// `runtime.asmcgocall` is the sanctioned way to run foreign (C-ABI) code on `g0`. It is
    /// invoked by `cgocall` precisely while the goroutine is `_Gsyscall`
    /// (<https://github.com/golang/go/blob/go1.26.4/src/runtime/cgocall.go#L167-L185>), and on the
    /// way back it **never touches `g.sched`** — it restores `rsp` from a saved
    /// `stack.hi - sp` depth stored on the `g0` stack
    /// (<https://github.com/golang/go/blob/go1.26.4/src/runtime/asm_amd64.s#L949-L1006>). This
    /// detour follows that contract: the only write to `g.sched` is the synthetic frame written by
    /// `gosave_systemstack_switch` (kept for stack-scan consistency, exactly as `asmcgocall` does).
    /// `_Gsyscall` goroutines are scanned via `g.syscallsp`/`syscallpc`/`syscallbp`, which we never
    /// touch, so this is safe.
    ///
    /// # Params
    ///
    /// * rdi - address of syscall number and arguments in memory.
    ///
    /// # Returns
    ///
    /// * rax - value returned by [`c_abi_wrapper`]
    #[unsafe(naked)]
    unsafe extern "C" fn experimental_c_abi_wrapper_on_systemstack() {
        naked_asm!(
            // Save rbp.
            // This is required, as we call `gosave_systemstack_switch` later.
            // Not having any local variables in this function before the call is required as well.
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
            // We are on the goroutine (curg) stack and must switch to g0.
            //
            // Compute our return depth BEFORE switching, the way `asmcgocall` does: instead of
            // restoring rsp from `g.sched` on the way back (the `systemstack` behavior that
            // corrupts a `_Gsyscall` g), we save `stack.hi - rbp` and recompute rsp from the
            // (re-read) `stack.hi` afterwards. Storing a depth rather than an absolute pointer
            // keeps us correct even if the goroutine stack is moved while we're on g0.
            //
            // r14 still holds curg here. g.stack.hi is at offset 0x8.
            // We hold the depth in r10: it must survive the `gosave_systemstack_switch` call below
            // (which smashes r9), so r9 can't be used here.
            // https://github.com/golang/go/blob/go1.26.4/src/runtime/runtime2.go#L401
            "mov    r10,[r14+0x8]",
            "sub    r10,rbp",
            // Save a synthetic frame into g.sched so that, if curg's stack is scanned while we run
            // on g0, it looks like we're parked in `systemstack_switch`. `gosave_systemstack_switch`
            // only clobbers r9/r14 and preserves rdi (our args pointer). rdx still holds m->g0.
            //
            // https://github.com/golang/go/blob/go1.26.4/src/runtime/asm_amd64.s#L984
            "mov    rsi,[rip + {gosave_systemstack_switch}]",
            "call   rsi",
            // Switch to g0: set TLS g and r14 (runtime code expects g in r14), load g0's sp.
            // The TLS slot for g sits at a link-mode-dependent offset from the FS base (see
            // `G_TLS_OFFSET`), so we address it through a register instead of a fixed displacement.
            // r11 is a safe scratch: this function never uses it, and it is caller-saved in the C
            // ABI, so neither the Go caller nor `c_abi_wrapper` expects it preserved. The live
            // values here (rdx=m->g0, rdi=args, r10=depth) are untouched.
            "mov    r11,[rip + {g_tls_offset}]",
            "mov    QWORD PTR fs:[r11],rdx",
            "mov    r14,rdx",
            "mov    rsp,[rdx+0x38]",
            // Reserve a 16-byte aligned slot on the g0 stack and stash the return depth there.
            // The g0 stack does not move, so this survives the call below.
            "sub    rsp,0x10",
            "and    rsp,-0x10",
            "mov    [rsp],r10",
            // rdi still points at the original syscall args (untouched by the switch).
            "call   {c_abi_wrapper}",
            // `c_abi_wrapper` left the syscall result in rax. It must survive until `ret`.
            "mov    r10,[rsp]",
            // Switch back to curg WITHOUT writing g.sched (the key difference from `systemstack`).
            // r14 currently holds g0; reach curg through m.
            "mov    rbx,[r14+0x30]",
            "mov    rsi,[rbx+0xb8]",
            // Restore the runtime's TLS g through the resolved offset (see the switch to g0 above).
            // r11 is scratch (unused elsewhere, caller-saved, freshly clobbered by the C call). rax
            // holds the syscall result and must survive to `ret`; it is untouched.
            "mov    r11,[rip + {g_tls_offset}]",
            "mov    QWORD PTR fs:[r11],rsi",
            "mov    r14,rsi",
            // Recompute our frame pointer: rbp == curg.stack.hi - depth. Re-read stack.hi in case
            // the stack moved. Then restore rsp to it and pop the saved rbp. rax (the syscall
            // result) is left untouched so it propagates to our caller.
            //
            // https://github.com/golang/go/blob/go1.26.4/src/runtime/asm_amd64.s#L1000-L1003
            "mov    rdx,[rsi+0x8]",
            "sub    rdx,r10",
            "mov    rsp,rdx",
            "pop    rbp",
            "ret",
            // Already on a system stack (g is gsignal or g0).
            "1:",
            // Tail call `c_abi_wrapper`. Pop rbp first.
            "pop    rbp",
            // At this point, rdi still stores the address of original syscall args in memory.
            // Call `c_abi_wrapper`.
            "jmp    {c_abi_wrapper}",
            // Current g is not gsignal, g0, nor curg.
            // Original implementation calls an internal panic routine.
            // For simplicity, we explicitly trigger an invalid instruction interrupt.
            "2:",
            "ud2",

            gosave_systemstack_switch = sym FN_GOSAVE_SYSTEMSTACK_SWITCH,
            c_abi_wrapper = sym c_abi_wrapper,
            g_tls_offset = sym G_TLS_OFFSET,
        );
    }

    /// Calls [`c_abi_syscall6_handler`].
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
    /// * rax - value returned by [`c_abi_syscall6_handler`].
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

    #[cfg(test)]
    mod tests {
        use super::find_g_tls_offset;

        /// The exact `mov <reg>, fs:[disp32]` / `mov fs:[disp32], <reg>` encodings the Go runtime
        /// uses to access `g`; `find_g_tls_offset` must recover the signed displacement from each.
        #[test]
        fn decodes_g_access_offsets() {
            // mov rdi, fs:[-0x3ce0] — externally-linked layout (large OpenSSL/librdkafka TLS).
            let external = [0x64, 0x48, 0x8b, 0x3c, 0x25, 0x20, 0xc3, 0xff, 0xff];
            assert_eq!(find_g_tls_offset(&external), Some(-0x3ce0));

            // mov rdi, fs:[-8] — the classic internally-linked layout.
            let internal = [0x64, 0x48, 0x8b, 0x3c, 0x25, 0xf8, 0xff, 0xff, 0xff];
            assert_eq!(find_g_tls_offset(&internal), Some(-8));

            // mov fs:[-0x3ce0], r14 — store form (opcode 0x89) with an extended reg (REX.R ->
            // 0x4c); exercises the opcode alternative and the `reg` field being ignored
            // in the ModRM mask.
            let store_r14 = [0x64, 0x4c, 0x89, 0x34, 0x25, 0x20, 0xc3, 0xff, 0xff];
            assert_eq!(find_g_tls_offset(&store_r14), Some(-0x3ce0));
        }

        /// The load is found even when it isn't the first instruction: the window slides past the
        /// prologue that real functions (e.g. `asmcgocall.abi0`) emit before loading `g`.
        #[test]
        fn scans_past_prologue() {
            // push rbp; mov rbp,rsp; mov rdi, fs:[-0x3ce0]
            let code = [
                0x55, 0x48, 0x89, 0xe5, // push rbp; mov rbp,rsp
                0x64, 0x48, 0x8b, 0x3c, 0x25, 0x20, 0xc3, 0xff, 0xff,
            ];
            assert_eq!(find_g_tls_offset(&code), Some(-0x3ce0));
        }

        /// Code without an `fs:[disp32]` access must not match — notably a stack spill
        /// (`mov [rsp+8], rbx`) has no FS prefix and a SIB base of rsp (0x24), not a bare
        /// disp32 (0x25).
        #[test]
        fn ignores_non_g_instructions() {
            // push rbp; mov rbp,rsp; sub rsp,0x18; mov [rsp+8], rbx
            let code = [
                0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x18, 0x48, 0x89, 0x5c, 0x24, 0x08,
            ];
            assert_eq!(find_g_tls_offset(&code), None);
        }
    }
}
