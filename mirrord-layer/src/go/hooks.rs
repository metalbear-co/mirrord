use std::arch::asm;

use frida_gum::interceptor::Interceptor;
use tracing::debug;

use crate::{macros::hook_symbol, socket::hooks::*};

// TODO: Add docs from before
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

/// Syscall handler: socket calls go to the socket detour, while rest are passed to libc::syscall.
#[no_mangle]
#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
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
        //10000 => 1, // Ask Aviram what is this for?
        _ => syscall_3(syscall, param1, param2, param3),
    };
    debug!("return -> {res:?}");
    return res;
}

/// libc's syscall doesn't return the value that go expects (it does translation)
#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
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

pub(crate) fn enable_socket_hooks(interceptor: &mut Interceptor, binary: &str) {
    hook_symbol!(
        interceptor,
        "syscall.RawSyscall.abi0",
        go_rawsyscall_detour,
        binary
    );
    todo!();
}
