use std::arch::asm;

use errno::errno;
use tracing::trace;

use crate::{
    close_detour, file::hooks::*, hooks::HookManager, macros::hook_symbol, socket::hooks::*,
    FILE_MODE,
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
#[naked]
unsafe extern "C" fn go_rawsyscall_detour() {
    asm!(
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
        "ret",
        options(noreturn)
    );
}

/// [Naked function] hook for Syscall6
#[naked]
unsafe extern "C" fn go_syscall6_detour() {
    asm!(
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
        "ret",
        options(noreturn)
    );
}

/// [Naked function] hook for Syscall
#[naked]
unsafe extern "C" fn go_syscall_detour() {
    asm!(
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
        "ret",
        options(noreturn)
    );
}

/// [Naked function] maps to gasave_systemstack_switch, called by asmcgocall.abi0
#[no_mangle]
#[naked]
unsafe extern "C" fn gosave_systemstack_switch() {
    asm!(
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
        "ret",
        options(noreturn)
    );
}

/// [Naked function] maps to runtime.abort.abi0, called by `gosave_systemstack_switch`
#[no_mangle]
#[naked]
unsafe extern "C" fn go_runtime_abort() {
    asm!("int 0x3", "jmp go_runtime_abort", options(noreturn));
}

/// Syscall & Rawsyscall handler - supports upto 4 params, used for socket,
/// bind, listen, and accept
/// Note: Depending on success/failure Syscall may or may not call this handler
#[no_mangle]
unsafe extern "C" fn c_abi_syscall_handler(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
) -> i64 {
    trace!(
        "c_abi_syscall_handler: syscall={} param1={} param2={} param3={}",
        syscall,
        param1,
        param2,
        param3
    );
    let res = match syscall {
        libc::SYS_socket => socket_detour(param1 as _, param2 as _, param3 as _) as i64,
        libc::SYS_bind => bind_detour(param1 as _, param2 as _, param3 as _) as i64,
        libc::SYS_listen => listen_detour(param1 as _, param2 as _) as i64,
        libc::SYS_connect => connect_detour(param1 as _, param2 as _, param3 as _) as i64,
        libc::SYS_accept => accept_detour(param1 as _, param2 as _, param3 as _) as i64,
        libc::SYS_close => close_detour(param1 as _) as i64,

        _ if FILE_MODE.get().unwrap().is_active() => match syscall {
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
            libc::SYS_getdents64 => getdents64_detour(param1 as _, param2 as _, param3 as _) as i64,
            _ => {
                let syscall_res = syscall_3(syscall, param1, param2, param3);
                return syscall_res;
            }
        },
        _ => {
            let syscall_res = syscall_3(syscall, param1, param2, param3);
            return syscall_res;
        }
    };
    match res {
        -1 => -errno().0 as i64,
        _ => res,
    }
}

/// [Naked function] 3 param version (Syscall6) for making the syscall, libc's syscall is not
/// used here as it doesn't return the value that go expects (it does translation)
#[naked]
unsafe extern "C" fn syscall_3(syscall: i64, param1: i64, param2: i64, param3: i64) -> i64 {
    asm!(
        "mov    rax, rdi",
        "mov    rdi, rsi",
        "mov    rsi, rdx",
        "mov    rdx, rcx",
        "syscall",
        "ret",
        options(noreturn)
    )
}

/// [Naked function] 6 param version, used by Rawsyscall & Syscall
#[naked]
unsafe extern "C" pub(crate) fn syscall_6(
    syscall: i64,
    param1: i64,
    param2: i64,
    param3: i64,
    param4: i64,
    param5: i64,
    param6: i64,
) -> i64 {
    asm!(
        "mov    rax, rdi",
        "mov    rdi, rsi",
        "mov    rsi, rdx",
        "mov    rdx, rcx",
        "mov    r10, r8",
        "mov    r8, r9",
        "mov    r9, QWORD PTR[rsp]",
        "syscall",
        "ret",
        options(noreturn)
    )
}

/// Clobbers rax, rcx
#[no_mangle]
#[naked]
unsafe extern "C" fn enter_syscall() {
    asm!(
        "mov rax, qword ptr [r14 + 0x30]", // get mp
        "inc qword ptr [ rax + 0x130 ]",   // inc cgocall
        "inc qword ptr [ rax + 0x138 ]",   // inc cgo
        "mov rcx, qword ptr [ rax + 0x140 ]",
        "mov qword ptr [rcx], 0x0",         // reset traceback
        "mov byte ptr [ RAX + 0x118], 0x1", // incgo = true
        "ret",
        options(noreturn)
    );
}

/// clobbers xmm15, r14, rax
#[no_mangle]
#[naked]
unsafe extern "C" fn exit_syscall() {
    asm!(
        "xorps xmm15, xmm15",
        "mov r14, qword ptr FS:[0xfffffff8]",
        "mov rax, qword ptr [r14 + 0x30]",
        "dec qword ptr [ rax + 0x138 ]",    // dec cgo
        "mov byte ptr [ RAX + 0x118], 0x0", // incgo = false
        "ret",
        options(noreturn)
    );
}

/// Detour for Go >= 1.19
/// On Go 1.19 one hook catches all (?) syscalls and therefore we call the syscall6 handler always
/// so syscall6 handler need to handle syscall3 detours as well.
#[naked]
unsafe extern "C" fn go_syscall_new_detour() {
    asm!(
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
        "jz 1f",
        "mov rax, qword ptr [rdi + 0x30]",
        "mov rsi, qword ptr [rax + 0x50]",
        "cmp rdi, rsi",
        "jz 1f",
        "mov rsi, qword ptr [rax]",
        "cmp rdi, rsi",
        "jz 1f",
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
        "jbe    2f",
        "neg    rax",
        "mov    rcx, rax",
        "mov    rax, -0x1",
        "mov    rbx, 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        // same as `nosave` in the asmcgocall.
        // calls the abi handler, when we have no g
        "1:",
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
        "jbe    2f",
        "neg    rax",
        "mov    rcx, rax",
        "mov    rax, -0x1",
        "mov    rbx, 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        "2:",
        // RAX already contains return value
        "mov    rbx, 0x0",
        "mov    rcx, 0x0",
        "xorps  xmm15, xmm15",
        "mov    r14, QWORD PTR FS:[0xfffffff8]",
        "ret",
        options(noreturn)
    )
}

/// Hooks for when hooking a pre go 1.19 binary
fn pre_go1_19(hook_manager: &mut HookManager) {
    hook_symbol!(
        hook_manager,
        "syscall.RawSyscall.abi0",
        go_rawsyscall_detour
    );
    hook_symbol!(hook_manager, "syscall.Syscall6.abi0", go_syscall6_detour);
    hook_symbol!(hook_manager, "syscall.Syscall.abi0", go_syscall_detour);
}

/// Hooks for when hooking a post go 1.19 binary
fn post_go1_19(hook_manager: &mut HookManager) {
    hook_symbol!(
        hook_manager,
        "runtime/internal/syscall.Syscall6",
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
            trace!("found version < 1.19");
            pre_go1_19(hook_manager);
        }
    }
}
