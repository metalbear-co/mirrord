//! Utilities to deal with in-process memory.

use std::mem::MaybeUninit;

use winapi::um::{memoryapi::VirtualQuery, winnt::{MEMORY_BASIC_INFORMATION, MEM_COMMIT}};

/// Queries the allocation data for the page which contains `ptr`.
/// 
/// # Arguments
/// 
/// * `ptr` - Pointer to memory. Should be an address corresponding to the process address space.
/// 
/// # Safety
/// 
/// You can provide absolutely any value for `ptr`, and this operation will not fail.
/// The memory is never dereferenced, or touched in any sense.
pub fn is_memory_valid<T>(ptr: *const T) -> bool {
    let mut buf: MEMORY_BASIC_INFORMATION = unsafe { MaybeUninit::zeroed().assume_init() };
    let ret = unsafe { VirtualQuery(ptr as _, &mut buf, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    if ret == 0 || ret != std::mem::size_of::<MEMORY_BASIC_INFORMATION>() {
        return false;
    }

	// https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-memory_basic_information
	// MEM_FREE and MEM_RESERVE are both invalid in this context.
    buf.State == MEM_COMMIT
}