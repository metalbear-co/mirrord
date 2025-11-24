use std::arch::naked_asm;

use tracing::trace;

use crate::{HookManager, go::c_abi_syscall6_handler, macros::hook_symbol};

type VoidFn = unsafe extern "C" fn() -> ();
static mut FN_ASMCGOCALL: Option<VoidFn> = None;

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
    unsafe {
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
}

/// Detour for Go >= 1.19
/// On Go 1.19 one hook catches all (?) syscalls and therefore we call the syscall6 handler always
/// so syscall6 handler need to handle syscall3 detours as well.
// We're using asmcogcall to avoid re-implementing it and doing it badly.
#[unsafe(naked)]
unsafe extern "C" fn go_syscall_new_detour() {
    naked_asm!(
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
        "b.cc 2f",
        // syscall fail flow
        "mov x4, -0x1",
        "str x4, [sp, 0x40]",
        "str xzr, [sp, 0x48]",
        "neg x0, x0",
        "str x0, [sp, 0x50]",
        "ret",
        // syscall success
        "2:",
        "str x0, [sp, 0x40]",
        "str x1, [sp, 0x48]",
        "str xzr, [sp, 0x50]",
        "ret",
        asmcgocall = sym FN_ASMCGOCALL,
        syscall_handler = sym mirrord_syscall_handler
    )
}

/// Hooks for when hooking a go binary between 1.19 and 1.23
fn post_go1_19(hook_manager: &mut HookManager, module_name: Option<&str>) {
    if let Some(module_name) = module_name {
        unsafe {
            FN_ASMCGOCALL = std::mem::transmute::<
                frida_gum::NativePointer,
                std::option::Option<unsafe extern "C" fn()>,
            >(
                hook_manager
                    .resolve_symbol_main_module(module_name, "runtime.asmcgocall")
                    .expect("found go but couldn't find runtime.asmcgocall please file a bug"),
            );
        }
        hook_symbol!(
            hook_manager,
            module_name,
            "runtime/internal/syscall.Syscall6.abi0",
            go_syscall_new_detour
        );
    } else {
        unsafe {
            FN_ASMCGOCALL = std::mem::transmute::<
                frida_gum::NativePointer,
                std::option::Option<unsafe extern "C" fn()>,
            >(
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
}

/// Hooks for when hooking a post go 1.23 binary
fn post_go1_23(hook_manager: &mut HookManager, module_name: Option<&str>) {
    if let Some(module_name) = module_name {
        unsafe {
            FN_ASMCGOCALL = std::mem::transmute::<
                frida_gum::NativePointer,
                std::option::Option<unsafe extern "C" fn()>,
            >(
                hook_manager
                    .resolve_symbol_in_module(module_name, "runtime.asmcgocall")
                    .expect("found go but couldn't find runtime.asmcgocall please file a bug"),
            );
        }
        hook_symbol!(
            hook_manager,
            module_name,
            "internal/runtime/syscall.Syscall6.abi0",
            go_syscall_new_detour
        );
    } else {
        unsafe {
            FN_ASMCGOCALL = std::mem::transmute::<
                frida_gum::NativePointer,
                std::option::Option<unsafe extern "C" fn()>,
            >(
                hook_manager
                    .resolve_symbol_main_module("runtime.asmcgocall")
                    .expect("found go but couldn't find runtime.asmcgocall please file a bug"),
            );
        }
        hook_symbol!(
            hook_manager,
            "internal/runtime/syscall.Syscall6.abi0",
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

    if version >= 1.23 {
        trace!("found version >= 1.23");
        post_go1_23(hook_manager, None);
    } else if version >= 1.19 {
        trace!("found version >= 1.19");
        post_go1_19(hook_manager, None);
    } else {
        trace!("found version < 1.19, arm64 not supported - not hooking");
    }

    if experimental.vfork_emulation {
        tracing::warn!("vfork emulation is not yet supported for Go programs on ARM64.");
    }
}

/// Same as [`enable_hooks`], but hook symbols found in the given `module_name`.
pub(crate) fn enable_hooks_in_loaded_module(
    hook_manager: &mut HookManager,
    module_name: String,
    experimental: &ExperimentalConfig,
) {
    let Some(version) = super::get_go_runtime_version_in_module(hook_manager, &module_name) else {
        return;
    };

    if version >= 1.23 {
        trace!("found version >= 1.23");
        post_go1_23(hook_manager, Some(module_name.as_str()));
    } else if version >= 1.19 {
        trace!("found version >= 1.19");
        post_go1_19(hook_manager, Some(module_name.as_str()));
    } else {
        trace!("found version < 1.19, arm64 not supported - not hooking");
    }
}
