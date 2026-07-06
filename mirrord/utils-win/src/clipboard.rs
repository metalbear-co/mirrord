//! Clipboard helper for copying text to the Windows clipboard.

use winapi::{
    shared::{minwindef::FALSE, windef::HWND},
    um::{
        winbase::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        winuser::{
            CF_UNICODETEXT, CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        },
    },
};

/// Copies a null-terminated wide string to the clipboard.
///
/// # Arguments
///
/// * `window` - the clipboard owner.
/// * `text` - the null-terminated wide text to copy.
pub fn set_text(window: HWND, text: &[u16]) {
    unsafe {
        if OpenClipboard(window) == FALSE {
            return;
        }
        EmptyClipboard();

        let handle = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(text));
        if !handle.is_null() {
            let destination = GlobalLock(handle) as *mut u16;
            if !destination.is_null() {
                std::ptr::copy_nonoverlapping(text.as_ptr(), destination, text.len());
                GlobalUnlock(handle);
                // Ownership of the memory passes to the clipboard on success.
                SetClipboardData(CF_UNICODETEXT, handle);
            }
        }

        CloseClipboard();
    }
}
