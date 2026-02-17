#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod go_1_24 {
    use std::arch::naked_asm;

    use crate::{hooks::HookManager, macros::hook_symbol};

    pub(crate) fn hook(hook_manager: &mut HookManager, module_name: &str) {
        hook_symbol!(
            hook_manager,
            module_name,
            "internal/runtime/syscall.Syscall6",
            internal_runtime_syscall_syscall6_detour
        );
    }

    /// Detour of `internal/runtime/syscall.Syscall6`.
    ///
    /// func Syscall6(num, a1, a2, a3, a4, a5, a6 uintptr) (r1, r2, errno uintptr)
    /// <https://github.com/golang/go/blob/go1.24.11/src/internal/runtime/syscall/asm_linux_amd64.s#L27>
    ///
    /// Function arguments are mapped as the following:
    /// rax = num
    /// rbx = a1
    /// rcx = a2
    /// rdi = a3
    /// rsi = a4
    /// r8  = a5
    /// r9  = a6
    /// r14 = g
    #[unsafe(naked)]
    unsafe extern "C" fn internal_runtime_syscall_syscall6_detour() {
        naked_asm!(
            // If the syscall is SYS_EXIT or SYS_EXIT_GROUP,
            // skip our logic and just execute it.
            "cmp    rax,60", // SYS_EXIT
            "je     2f",
            "cmp    rax,231", // SYS_EXIT_GROUP
            "je     2f",
            "push   rbp",
            "mov    rbp,rsp",
            // Reservce space and store args.
            "sub    rsp,0x40",
            "mov    [rsp+0x38],r9",
            "mov    [rsp+0x30],r8",
            "mov    [rsp+0x28],rsi",
            "mov    [rsp+0x20],rdi",
            "mov    [rsp+0x18],rcx",
            "mov    [rsp+0x10],rbx",
            "mov    [rsp],rax",
            // function args starting at rdi.
            "mov   rdi,rsp",
            "call   {c_abi_wrapper_on_systemstack}",
            // Check failure
            "cmp    rax,-0xfff",
            "jbe    1f",
            // Syscall failed.
            // Save errno in rcx.
            "neg    rax",
            "mov    rcx,rax",
            // Fill -1 in rax.
            "mov    rax,-0x1",
            // Clear result register.
            "mov    rbx,0x0",
            // Drop space reserved for locals.
            "add    rsp,0x40",
            // Restore rbp and return.
            "pop    rbp",
            "ret",
            // Syscall did not fail. Result is in rax. Clear other result registers.
            "1:",
            "mov    rbx,0x0",
            "mov    rcx,0x0",
            // Drop space for storing args locally.
            "add    rsp,0x40",
            // Restore rbp and return.
            "pop    rbp",
            "ret",
            // Move the first syscall argument to the correct register (rdx),
            // and execute the syscall.
            "2:",
            "mov    rdx,rdi",
            "syscall",

            c_abi_wrapper_on_systemstack = sym c_abi_wrapper_on_systemstack,
        );
    }

    /// Calls [`c_abi_wrapper`] on systemstack.
    ///
    /// Implemented based on [`runtime.systemstack`](https://github.com/golang/go/blob/go1.24.11/src/runtime/asm_amd64.s#L483)
    #[unsafe(naked)]
    unsafe extern "C" fn c_abi_wrapper_on_systemstack() {
        naked_asm!(
            // This is required, as we call `gosave_systemstack_switch` later.
            // Not having any local variables in this function is required as well.
            "push   rbp",
            "mov    rbp,rsp",
            // We assume that r14 stores the address of g.
            // Load address of current m to rbx.
            // https://github.com/golang/go/blob/go1.24.11/src/runtime/runtime2.go#L410
            "mov    rbx,[r14+0x30]",
            // Check if g is m->gsignal. If so, do not switch stack.
            // https://github.com/golang/go/blob/go1.24.11/src/runtime/runtime2.go#L536
            "cmp    r14,[rbx+0x50]",
            "je     1f",
            // Load address of m->g0 to rdx.
            // Check if g is g0. If so, do not switch stack.
            "mov    rdx,[rbx]",
            "cmp    r14,rdx",
            "je     1f",
            // We expect g is m->curg now. If not, abort with `ud2`.
            // https://github.com/golang/go/blob/go1.24.11/src/runtime/runtime2.go#L541
            "cmp    r14,[rbx+0xc0]",
            "jne    2f",
            // Switch to system stack.
            "call    {gosave_systemstack_switch}",
            // rdx holds the address of m->g0. Store it also in TLS and r14.
            "mov    fs:0xfffffffffffffff8,rdx",
            "mov    r14,rdx",
            // Fill rsp with g0's g->sched->sp. After this, we are on system stack.
            "mov    rsp,[rdx+0x38]",
            // We assume rdi still has the original syscall args stored on user g stack.
            "call   {c_abi_wrapper}",
            // Switch back to g stack.
            // https://github.com/golang/go/blob/go1.24.11/src/runtime/asm_amd64.s#L516-L526
            // We assume r14 still has g0. Store g0's m in rbx.
            "mov    rbx,[r14+0x30]",
            // Store m->curg in rsi.
            "mov    rsi,[rbx+0xc0]",
            // Store user g in TLS
            "mov    fs:0xfffffffffffffff8,rsi",
            // Store user g in r14
            "mov    r14,rsi",
            // Fill rsp with g->sched->sp
            "mov    rsp,[rsi+0x38]",
            // Fill rbp with g->sched->bp
            // https://github.com/golang/go/blob/go1.24.11/src/runtime/runtime2.go#L317
            "mov    rbp,[rsi+0x68]",
            "mov    QWORD PTR [rsi+0x38],0x0",
            "mov    QWORD PTR [rsi+0x68],0x0",
            // Restore rbp and return.
            "pop    rbp",
            "ret",
            // Already on system stack. Tail call `c_abi_wrapper`.
            // https://github.com/golang/go/blob/go1.24.11/src/runtime/asm_amd64.s#L528-L537
            "1:",
            "pop    rbp",
            "jmp    {c_abi_wrapper}",
            // Abort the program.
            "2:",
            "ud2",
            gosave_systemstack_switch = sym gosave_systemstack_switch,
            c_abi_wrapper = sym c_abi_wrapper,
        );
    }

    /// Exact copy of the `go_1_25::c_abi_wrapper` function.
    ///
    /// Move function args from stack to registers so we can call
    /// [`c_abi_syscall6_handler`](crate::go::c_abi_syscall6_handler).
    ///
    /// We expect rdi stores the address of the start of function args on the stack.
    ///
    /// C ABI: fn(rdi, rsi, rdx, rcx, r8, r9, stack)
    #[unsafe(naked)]
    unsafe extern "C" fn c_abi_wrapper() {
        naked_asm!(
            "push   rbp",
            "mov    rbp,rsp",
            "sub    rsp,0x8",
            "and    rsp,-0x10",
            "mov    rax,[rdi]",
            "mov    rsi,[rdi+0x10]",
            "mov    rdx,[rdi+0x18]",
            "mov    rcx,[rdi+0x20]",
            "mov    r8,[rdi+0x28]",
            "mov    r9,[rdi+0x30]",
            "mov    r10,[rdi+0x38]",
            "mov    [rsp],r10",
            "mov    rdi,rax",
            "call   c_abi_syscall6_handler",
            "mov    rsp, rbp",
            "pop    rbp",
            "ret"
        );
    }

    /// Implemented based on [`gosave_systemstack_switch`](https://github.com/golang/go/blob/go1.24.11/src/runtime/asm_amd64.s#L823)
    ///
    /// Smashes r9.
    #[unsafe(naked)]
    unsafe extern "C" fn gosave_systemstack_switch() {
        naked_asm!(
            // TODO: In Go's implementation, it loads the address of `runtime.systemstack_switch` + 8 bytes.
            // [`runtime.systemstack_switch`](https://github.com/golang/go/blob/go1.24.11/src/runtime/asm_amd64.s#L475)
            // is a dummy marker function. If it is directly called, it will hit a `ud2` trap.
            // Go only cares that g->sched->pc is set to an address between the prologue and epilogue.
            //
            // I don't know if it matters for the address to be between the actual `systemstack_switch` function.
            // Or a simple dummy function will work?
            "lea    r9, [rip + {dummy} + 0x8]",
            // Store r9 in g->sched->pc.
            "mov    QWORD PTR [r14+0x40],r9",
            "lea    r9, [rsp+0x8]",
            // Store r9 in g->sched->sp.
            "mov    QWORD PTR [r14+0x38],r9",
            // Store 0 in g->sched->ret.
            "mov    QWORD PTR [r14+0x58],0x0",
            // Store rbp in g->sched->bp.
            "mov    QWORD PTR [r14+0x68],rbp",
            // Store g->sched->ctxt in r9.
            "mov    r9, QWORD PTR [r14+0x50]",
            // If g->sched->ctxt == 0 abort runtime, otherwise return.
            "test   r9, r9",
            "jz     1f",
            "call   {runtime_abort}",
            "1:",
            "ret",

            runtime_abort = sym runtime_abort,
            dummy = sym dummy,
        );
    }

    /// Implemented based on [`runtime.abort`](https://github.com/golang/go/blob/fed3b0a298464457c58d1150bdb3942f22bd6220/src/runtime/asm_amd64.s#L1237)
    ///
    /// This function crashes the runtime. `int3` is recognized by debuggers.
    /// The rest of the function is an infinite trap loop.
    #[unsafe(naked)]
    unsafe extern "C" fn runtime_abort() {
        naked_asm!("int3", "1:", "jmp 1b",);
    }

    /// A dummy function that wraps some `nop` between prologue and epilogue.
    #[unsafe(naked)]
    unsafe extern "C" fn dummy() {
        naked_asm!(
            "push   rbp",
            "mov    rbp,rsp",
            "nop",
            "nop",
            "nop",
            "nop",
            "nop", // this is the address we save in g->sched->pc
            "nop",
            "nop",
            "pop    rbp",
            "ret",
        );
    }
}
