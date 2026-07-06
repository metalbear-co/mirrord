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

/// Fail-fast / stack-buffer-overrun exception (`__fastfail`, `abort`). Bypasses user handlers;
/// listed only so the handler can name it.
const STATUS_FAIL_FAST_EXCEPTION: DWORD = ntstatus::STATUS_STACK_BUFFER_OVERRUN as u32;
/// Heap corruption. Bypasses user handlers; listed only so the handler can name it.
const STATUS_HEAP_CORRUPTION: DWORD = ntstatus::STATUS_HEAP_CORRUPTION as u32;

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
pub(super) fn is_severe(code: DWORD) -> bool {
    matches!(
        code,
        EXCEPTION_ACCESS_VIOLATION
            | EXCEPTION_STACK_OVERFLOW
            | EXCEPTION_ILLEGAL_INSTRUCTION
            | STATUS_FAIL_FAST_EXCEPTION
            | STATUS_HEAP_CORRUPTION
    )
}

/// Returns a short human name for an exception code.
pub(super) fn code_name(code: DWORD) -> &'static str {
    match code {
        EXCEPTION_ACCESS_VIOLATION => "access violation",
        EXCEPTION_STACK_OVERFLOW => "stack overflow",
        EXCEPTION_ILLEGAL_INSTRUCTION => "illegal instruction",
        STATUS_FAIL_FAST_EXCEPTION => "fail-fast",
        STATUS_HEAP_CORRUPTION => "heap corruption",
        _ => "exception",
    }
}
