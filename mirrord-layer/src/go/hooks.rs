#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
pub(crate) mod go_hooks {
    use std::arch::asm;

    use frida_gum::interceptor::Interceptor;
    use tracing::debug;

    use crate::{macros::hook_symbol, socket::hooks::*};

    #[naked]
    unsafe extern "C" fn go_rawsyscall_detour() {
        asm!(
            "mov rbx, QWORD PTR [rsp+0x10]",
            "mov r10, QWORD PTR [rsp+0x18]",
            "mov rcx, QWORD PTR [rsp+0x20]",
            "mov rax, QWORD PTR [rsp+0x8]",
            "mov    rdx, rsp",
            "mov    rdi, QWORD PTR fs:[0xfffffff8]",
            "cmp    rdi, 0x0",
            "je     2f",
            "mov    r8, QWORD PTR [rdi+0x30]",
            "mov    rsi, QWORD PTR [r8+0x50]",
            "cmp    rdi,rsi",
            "je     2f",
            "mov    rsi,QWORD PTR [r8]",
            "cmp    rdi,rsi",
            "je     2f",
            "call   mirrord_go_systemstack_switch",
            "mov    QWORD PTR fs:[0xfffffff8], rsi",
            "mov    rsp,QWORD PTR [rsi+0x38]",
            "sub    rsp,0x40",
            "and    rsp,0xfffffffffffffff0",
            "mov    QWORD PTR [rsp+0x30],rdi",
            "mov    rdi,QWORD PTR [rdi+0x8]",
            "sub    rdi,rdx",
            "mov    QWORD PTR [rsp+0x28],rdi",
            "mov    rsi, rbx",
            "mov    rdx, r10",
            "mov    rdi, rax",
            "call   c_abi_syscall_handler",
            "mov    rdi,QWORD PTR [rsp+0x30]",
            "mov    rsi,QWORD PTR [rdi+0x8]",
            "sub    rsi,QWORD PTR [rsp+0x28]",
            "mov    QWORD PTR fs:0xfffffff8, rdi",
            "mov    rsp,rsi",
            "cmp    rax, -0xfff",
            "jbe    3f",
            "mov    QWORD PTR [rsp+0x28], -0x1",
            "mov    QWORD PTR [rsp+0x30], 0x0",
            "neg    rax",
            "mov    QWORD PTR [rsp+0x38], rax",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            "2:",
            "sub    rsp,0x40",
            "and    rsp,0xfffffffffffffff0",
            "mov    QWORD PTR [rsp+0x30],0x0",
            "mov    QWORD PTR [rsp+0x28],rdx",
            "mov    rsi, rbx",
            "mov    rdx, r10",
            "mov    rdi, rax",
            "call   c_abi_syscall_handler",
            "mov    rsi,QWORD PTR [rsp+0x28]",
            "mov    rsp,rsi",
            "cmp    rax, -0xfff",
            "jbe    3f",
            "mov    QWORD PTR [rsp+0x28], -0x1",
            "mov    QWORD PTR [rsp+0x30], 0x0",
            "neg    rax",
            "mov    QWORD PTR [rsp+0x38], rax",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            "3:",
            "mov    QWORD PTR [rsp+0x28], rax",
            "mov    QWORD PTR [rsp+0x30], 0x0",
            "mov    QWORD PTR [rsp+0x38], 0x0",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            options(noreturn)
        );
    }

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
            "mov    rsi,QWORD PTR [r8]",
            "cmp    rdi,rsi",
            "je     2f",
            "call   mirrord_go_systemstack_switch",
            "mov    QWORD PTR fs:[0xfffffff8], rsi",
            "mov    rsp,QWORD PTR [rsi+0x38]",
            "sub    rsp,0x40",
            "and    rsp,0xfffffffffffffff0",
            "mov    QWORD PTR [rsp+0x30],rdi",
            "mov    rdi,QWORD PTR [rdi+0x8]",
            "sub    rdi,rdx",
            "mov    QWORD PTR [rsp+0x28],rdi",
            "mov    rsi, rbx",
            "mov    rdx, r10",
            "mov    rdi, rax",
            "mov    r8, r11",
            "mov    r9, r12",
            "mov     qword ptr [rsp], r13",
            "call   c_abi_syscall6_handler",
            "mov    rdi,QWORD PTR [rsp+0x30]",
            "mov    rsi,QWORD PTR [rdi+0x8]",
            "sub    rsi,QWORD PTR [rsp+0x28]",
            "mov    QWORD PTR fs:0xfffffff8, rdi",
            "mov    rsp,rsi",
            "cmp    rax, -0xfff",
            "jbe    3f",
            "mov    QWORD PTR [rsp+0x40], -0x1",
            "mov    QWORD PTR [rsp+0x48], 0x0",
            "neg    rax",
            "mov    QWORD PTR [rsp+0x50], rax",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            "2:",
            "sub    rsp,0x40",
            "and    rsp,0xfffffffffffffff0",
            "mov    QWORD PTR [rsp+0x30],0x0",
            "mov    QWORD PTR [rsp+0x28],rdx",
            "mov    rsi, rbx",
            "mov    rdx, r10",
            "mov    rdi, rax",
            "mov    r8, r11",
            "mov    r9, r12",
            "mov     qword ptr [rsp], r13",
            "call   c_abi_syscall6_handler",
            "mov    rsi,QWORD PTR [rsp+0x28]",
            "mov    rsp,rsi",
            "cmp    rax, -0xfff",
            "jbe    3f",
            "mov    QWORD PTR [rsp+0x40], -0x1",
            "mov    QWORD PTR [rsp+0x48], 0x0",
            "neg    rax",
            "mov    QWORD PTR [rsp+0x50], rax",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            "3:",
            "mov    QWORD PTR [rsp+0x40], rax",
            "mov    QWORD PTR [rsp+0x48], 0x0",
            "mov    QWORD PTR [rsp+0x50], 0x0",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            options(noreturn)
        );
    }

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
            "mov    rsi,QWORD PTR [r8]",
            "cmp    rdi,rsi",
            "je     2f",
            "call   mirrord_go_systemstack_switch",
            "mov    QWORD PTR fs:[0xfffffff8], rsi",
            "mov    rsp,QWORD PTR [rsi+0x38]",
            "sub    rsp,0x40",
            "and    rsp,0xfffffffffffffff0",
            "mov    QWORD PTR [rsp+0x30],rdi",
            "mov    rdi,QWORD PTR [rdi+0x8]",
            "sub    rdi,rdx",
            "mov    QWORD PTR [rsp+0x28],rdi",
            "mov    rsi, rbx",
            "mov    rdx, r10",
            "mov    rdi, rax",
            "call   c_abi_syscall_handler",
            "mov    rdi,QWORD PTR [rsp+0x30]",
            "mov    rsi,QWORD PTR [rdi+0x8]",
            "sub    rsi,QWORD PTR [rsp+0x28]",
            "mov    QWORD PTR fs:0xfffffff8, rdi",
            "mov    rsp,rsi",
            "cmp    rax, -0xfff",
            "jbe    3f",
            "mov    QWORD PTR [rsp+0x28], -0x1",
            "mov    QWORD PTR [rsp+0x30], 0x0",
            "neg    rax",
            "mov    QWORD PTR [rsp+0x38], rax",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            "2:",
            "sub    rsp,0x40",
            "and    rsp,0xfffffffffffffff0",
            "mov    QWORD PTR [rsp+0x30],0x0",
            "mov    QWORD PTR [rsp+0x28],rdx",
            "mov    rsi, rbx",
            "mov    rdx, r10",
            "mov    rdi, rax",
            "mov    r8, r11",
            "mov    r9, r12",
            "mov     qword ptr [rsp], r13",
            "call   c_abi_syscall6_handler",
            "mov    rsi,QWORD PTR [rsp+0x28]",
            "mov    rsp,rsi",
            "cmp    rax, -0xfff",
            "jbe    3f",
            "mov    QWORD PTR [rsp+0x28], -0x1",
            "mov    QWORD PTR [rsp+0x30], 0x0",
            "neg    rax",
            "mov    QWORD PTR [rsp+0x38], rax",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            "3:",
            "mov    QWORD PTR [rsp+0x28], rax",
            "mov    QWORD PTR [rsp+0x30], 0x0",
            "mov    QWORD PTR [rsp+0x38], 0x0",
            "xorps  xmm15,xmm15",
            "mov    r14, qword ptr FS:[0xfffffff8]",
            "ret",
            options(noreturn)
        );
    }

    #[no_mangle]
    #[naked]
    unsafe extern "C" fn mirrord_go_systemstack_switch() {
        asm!(
            "lea    r9,[rip+0xdd9]",
            "mov    QWORD PTR [r14+0x40],r9",
            "lea    r9,[rsp+0x8]",
            "mov    QWORD PTR [r14+0x38],r9",
            "mov    QWORD PTR [R14 + 0x58],0x0",
            "mov    QWORD PTR [r14+0x68],rbp",
            "mov    r9,QWORD PTR [r14+0x50]",
            "test   r9,r9",
            "jz     4f",
            "call   mirrord_go_runtime_abort",
            "4:",
            "ret",
            options(noreturn)
        );
    }

    #[no_mangle]
    #[naked]
    unsafe extern "C" fn mirrord_go_runtime_abort() {
        asm!("int 0x3", "jmp mirrord_go_runtime_abort", options(noreturn));
    }

    /// Syscall handler: socket calls go to the socket detour, while rest are passed to
    /// libc::syscall.
    #[no_mangle]
    unsafe extern "C" fn c_abi_syscall_handler(
        syscall: i64,
        param1: i64,
        param2: i64,
        param3: i64,
    ) -> i64 {
        debug!("C ABI handler received `Syscall - {:?}` with args >> arg1 -> {:?}, arg2 -> {:?}, arg3 -> {:?}",syscall, param1, param2, param3);
        let res = match syscall {
            libc::SYS_socket => socket_detour(param1 as i32, param2 as i32, param3 as i32) as i64,
            libc::SYS_bind => bind_detour(
                param1 as i32,
                param2 as *const libc::sockaddr,
                param3 as u32,
            ) as i64, //TODO: check if this argument passing is right?
            libc::SYS_listen => listen_detour(param1 as i32, param2 as i32) as i64,
            libc::SYS_accept4 => accept_detour(
                param1 as i32,
                param2 as *mut libc::sockaddr,
                param3 as *mut u32,
            ) as i64,
            _ => syscall_3(syscall, param1, param2, param3),
        };
        debug!("return -> {res:?}");
        res
    }

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
        debug!("C ABI handler received `Syscall6 - {:?}` with args >> arg1 -> {:?}, arg2 -> {:?}, arg3 -> {:?}, arg4 -> {:?}, arg5 -> {:?}, arg6 -> {:?}", syscall, param1, param2, param3, param4, param5, param6);
        let res = match syscall {
            10000 => 1,
            _ => syscall_6(syscall, param1, param2, param3, param4, param5, param6),
        };
        debug!("return -> {res:?}");
        res
    }

    /// libc's syscall doesn't return the value that go expects (it does translation)    
    #[naked]
    unsafe extern "C" fn syscall_3(syscall: i64, arg1: i64, arg2: i64, arg3: i64) -> i64 {
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

    #[naked]
    unsafe extern "C" fn syscall_6(
        syscall: i64,
        arg1: i64,
        arg2: i64,
        arg3: i64,
        arg4: i64,
        arg5: i64,
        arg6: i64,
    ) -> i64 {
        asm!(
            "mov    rax, rdi",
            "mov    rdi, rsi",
            "mov    rsi, rdx",
            "mov    rdx, rcx",
            "mov    r10, r8",
            "mov    r8, r9",
            "mov    r9, qword ptr[rsp]",
            "syscall",
            "ret",
            options(noreturn)
        )
    }

    pub(crate) fn enable_socket_hooks(interceptor: &mut Interceptor, binary: &str) {
        hook_symbol!(
            interceptor,
            "syscall.RawSyscall.abi0",
            go_rawsyscall_detour,
            binary
        );
        hook_symbol!(
            interceptor,
            "syscall.Syscall6.abi0",
            go_syscall6_detour,
            binary
        );
        hook_symbol!(
            interceptor,
            "syscall.Syscall.abi0",
            go_syscall_detour,
            binary
        );
    }
}
