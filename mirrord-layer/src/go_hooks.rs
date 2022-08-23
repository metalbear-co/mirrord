#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
pub(crate) mod hooks {
    use std::{arch::asm, sync::OnceLock};

    use errno::errno;
    use frida_gum::interceptor::Interceptor;
    use tracing::{error, trace};

    use crate::{close_detour, file::hooks::*, macros::hook_symbol, socket::hooks::*};
    static ENABLED_FILE_OPS: OnceLock<bool> = OnceLock::new();
    /*
     * Reference for which syscalls are managed by the handlers:
     * SYS_openat: Syscall6
     * SYS_read, SYS_write, SYS_lseek, SYS_faccessat: Syscall
     *
     * SYS_socket, SYS_bind, SYS_listen, SYS_accept, SYS_close: Syscall
     * SYS_accept4: Syscall6
     */

    /// [Naked function] This detour is taken from `runtime.asmcgocall.abi0`
    /// Refer: https://go.googlesource.com/go/+/refs/tags/go1.19rc2/src/runtime/asm_amd64.s#806
    /// Golang's assembler - https://go.dev/doc/asm
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
            "call   go_systemstack_switch",
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
            "call   go_systemstack_switch",
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
            "call   go_systemstack_switch",
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
    unsafe extern "C" fn go_systemstack_switch() {
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

    /// [Naked function] maps to runtime.abort.abi0, called by `go_systemstack_switch`
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
            libc::SYS_accept => accept_detour(param1 as _, param2 as _, param3 as _) as i64,
            libc::SYS_close => close_detour(param1 as _) as i64,

            _ if *ENABLED_FILE_OPS.get().unwrap() => match syscall {
                libc::SYS_read => read_detour(param1 as _, param2 as _, param3 as _) as i64,
                libc::SYS_write => write_detour(param1 as _, param2 as _, param3 as _) as i64,
                libc::SYS_lseek => lseek_detour(param1 as _, param2 as _, param3 as _) as i64,
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
            _ => res as i64,
        }
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
            _ if *ENABLED_FILE_OPS.get().unwrap() => match syscall {
                libc::SYS_openat => openat_detour(param1 as _, param2 as _, param3 as _) as i64,
                _ => {
                    let syscall_res =
                        syscall_6(syscall, param1, param2, param3, param4, param5, param6);
                    return syscall_res;
                }
            },
            _ => {
                let syscall_res =
                    syscall_6(syscall, param1, param2, param3, param4, param5, param6);
                return syscall_res;
            }
        };
        match res {
            -1 => -errno().0 as i64,
            _ => res as i64,
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

    /// Note: We only hook "RawSyscall", "Syscall6", and "Syscall" because for our usecase,
    /// when testing with "Gin", only these symbols were used to make syscalls.
    /// Refer:
    ///   - File zsyscall_linux_amd64.go generated using mksyscall.pl.
    ///   - https://cs.opensource.google/go/go/+/refs/tags/go1.18.5:src/syscall/syscall_unix.go
    pub(crate) fn enable_socket_hooks(
        interceptor: &mut Interceptor,
        binary: &str,
        enabled_file_ops: bool,
    ) {
        ENABLED_FILE_OPS
            .set(enabled_file_ops)
            .map_err(|err| {
                error!("Error setting ENABLED_FILE_OPS: {}", err);
            })
            .unwrap();

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
