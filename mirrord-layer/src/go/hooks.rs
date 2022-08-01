use std::arch::asm;

use frida_gum::interceptor::Interceptor;

use crate::socket::ops::socket;

#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
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

pub(crate) fn enable_socket_hooks(interceptor: &mut Interceptor, binary: &str) {
    todo!();
}
