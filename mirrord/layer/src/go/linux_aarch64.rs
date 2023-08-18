use std::arch::asm;

use tracing::trace;

use crate::{go::c_abi_syscall6_handler, macros::hook_symbol, HookManager};

type VoidFn = unsafe extern "C" fn() -> ();
static mut FN_ASMCGOCALL: Option<VoidFn> = None;

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

/// asmcgocall can only pass a pointer argument, so it sends us the SP
/// which layouts to this struct.
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

/// asmcgocall can pass a pointer, so this is a conversion call to `c_abi_syscall6_handler`
unsafe extern "C" fn mirrord_syscall_handler(syscall_struct: *const SyscallArgs) -> i64 {
    c_abi_syscall6_handler(
        (*syscall_struct).syscall,
        (*syscall_struct).arg1,
        (*syscall_struct).arg2,
        (*syscall_struct).arg3,
        (*syscall_struct).arg4,
        (*syscall_struct).arg5,
        (*syscall_struct).arg6,
    )
}

/// Detour for Go >= 1.19
/// On Go 1.19 one hook catches all (?) syscalls and therefore we call the syscall6 handler always
/// so syscall6 handler need to handle syscall3 detours as well.
// We're using asmcogcall to avoid re-implementing it and doing it badly.
#[naked]
unsafe extern "C" fn go_syscall_new_detour() {
    asm!(
        // save fp and lr to stack and reserve that memory.
        // if we don't do it and remember to load it back before ret
        // we will have unfortunate results
        "str x30, [sp, -0x10]!",
        "stur x29, [sp, -0x8]",
        "sub x29, sp, 0x8",
        // x1 should point to original sp+8, which should be the first parameter
        "add x1, sp, 0x18",
        // load mirrord_syscall_handler as first argument (x0)
        "adr    x0, {syscall_handler}",
        "adrp    x8, {asmcgocall}",
        "ldr     x8, [x8, :lo12:{asmcgocall}]",
        "blr     x8",
        // result is in x0
        // clean frame, this is a bit ugly - consider doing it at the end and just adjusting the return code offsets.
        "ldp x29, x30, [sp, -0x8]",
        "add sp, sp ,0x10",
        // check return code
        "cmn x0, 0xfff",
        // jump to success if return code == 0
        "b.cc 1f",
        // syscall fail flow
        "mov x4, -0x1",
        "str x4, [sp, 0x40]",
        "str xzr, [sp, 0x48]",
        "neg x0, x0",
        "str x0, [sp, 0x50]",
        "ret",
        // syscall success
        "1:",
        "str x0, [sp, 0x40]",
        "str x1, [sp, 0x48]",
        "str xzr, [sp, 0x50]",
        "ret",
        asmcgocall = sym FN_ASMCGOCALL,
        syscall_handler = sym mirrord_syscall_handler,
        options(noreturn)
    )
}

/// Hooks for when hooking a post go 1.19 binary
fn post_go1_19(hook_manager: &mut HookManager) {
    unsafe {
        FN_ASMCGOCALL = std::mem::transmute(
            hook_manager
                .resolve_symbol_main_module("runtime.asmcgocall")
                .expect("found go but couldn't find runtime.asmcgocall please file a bug"),
        );
    }
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
