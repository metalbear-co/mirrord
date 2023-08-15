use std::arch::asm;

use tracing::trace;

use crate::{macros::hook_symbol, HookManager};

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

/// [Naked function] 6 param version, used by Rawsyscall & Syscall
#[naked]
pub(crate) unsafe extern "C" fn syscall_6(
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
