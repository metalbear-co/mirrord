//! Exception-code classification and naming.
//!
//! The crash handler decides whether a fault is worth acting on and what to call it. These helpers
//! keep that mapping in one place — the named severe codes, the fail-fast and heap codes that
//! bypass user handlers, and the short human names — separate from the handler's control flow.

use winapi::{
    shared::{minwindef::DWORD, ntstatus},
    um::{
        minwinbase::{
            EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_STACK_OVERFLOW,
        },
        winnt::EXCEPTION_POINTERS,
    },
};

/// Reads the exception code from the pointers, guarding against nulls.
///
/// # Safety
///
/// `info` must be null or a valid `EXCEPTION_POINTERS` supplied by the OS.
pub(super) unsafe fn exception_code(info: *mut EXCEPTION_POINTERS) -> DWORD {
    if info.is_null() {
        return 0;
    }
    let exception = unsafe { (*info).ExceptionRecord };
    if exception.is_null() {
        return 0;
    }
    unsafe { (*exception).ExceptionCode }
}

/// Reports whether a code is one of the named severe codes (used for first-chance logging).
///
/// The fail-fast / stack-buffer-overrun (`__fastfail`, `abort`) and heap-corruption codes bypass
/// user handlers; they come from `ntstatus` as `i32`, so they are cast to `u32` in place rather
/// than aliased to named constants.
pub(super) fn is_severe(code: DWORD) -> bool {
    matches!(
        code,
        EXCEPTION_ACCESS_VIOLATION | EXCEPTION_STACK_OVERFLOW | EXCEPTION_ILLEGAL_INSTRUCTION
    ) || code == ntstatus::STATUS_STACK_BUFFER_OVERRUN as u32
        || code == ntstatus::STATUS_HEAP_CORRUPTION as u32
}

/// Returns a short human name for an exception code.
pub(super) fn code_name(code: DWORD) -> &'static str {
    match code {
        EXCEPTION_ACCESS_VIOLATION => "access violation",
        EXCEPTION_STACK_OVERFLOW => "stack overflow",
        EXCEPTION_ILLEGAL_INSTRUCTION => "illegal instruction",
        _ if code == ntstatus::STATUS_STACK_BUFFER_OVERRUN as u32 => "fail-fast",
        _ if code == ntstatus::STATUS_HEAP_CORRUPTION as u32 => "heap corruption",
        _ => "exception",
    }
}
