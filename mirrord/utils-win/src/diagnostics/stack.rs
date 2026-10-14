//! Stack walks with frame sizes.
//!
//! A stack overflow names the function that ran out of stack, which is rarely the one that used it
//! up. What answers that is the size of every frame on the way down. [`walk`] recovers the frames
//! from the x64 unwind data, and a frame's size is the distance from its stack pointer to its
//! caller's: the numbers WinDbg's `kf` prints.
//!
//! [`walk`] is allocation-free, so the in-process crash handler can run it on a broken thread.

use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, RtlLookupFunctionEntry, RtlVirtualUnwind, UNW_FLAG_NHANDLER,
};

/// One frame of a walked stack.
#[derive(Clone, Copy, Default)]
pub struct Frame {
    /// The faulting instruction for the innermost frame, a return address for every other one.
    pub ip: usize,
    /// The stack pointer inside this frame.
    pub sp: usize,
}

/// Walks the stack that `context` describes, innermost frame first, into `frames`.
///
/// Allocation-free. Stops at the outermost frame, when `frames` is full, or where the unwind data
/// leads somewhere a caller cannot be.
///
/// # Safety
///
/// `context` must describe a stack of this process that does not change during the walk: the
/// calling thread's own, or that of a thread blocked until the walk is done.
///
/// # Returns
///
/// How many frames were written.
pub unsafe fn walk(context: &CONTEXT, frames: &mut [Frame]) -> usize {
    let mut context = *context;
    let mut count = 0;

    for slot in frames.iter_mut() {
        if context.Rip == 0 {
            break;
        }
        *slot = Frame {
            ip: context.Rip as usize,
            sp: context.Rsp as usize,
        };
        count += 1;

        let callee_sp = context.Rsp;
        let mut image_base = 0;
        let entry =
            unsafe { RtlLookupFunctionEntry(context.Rip, &mut image_base, std::ptr::null_mut()) };
        if entry.is_null() {
            // A leaf function has no unwind data because it never moves rsp, so its return
            // address is on top of the stack.
            context.Rip = unsafe { *(context.Rsp as *const u64) };
            context.Rsp += 8;
        } else {
            let mut handler_data = std::ptr::null_mut();
            let mut establisher_frame = 0;
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    image_base,
                    context.Rip,
                    entry,
                    &mut context,
                    &mut handler_data,
                    &mut establisher_frame,
                    std::ptr::null_mut(),
                )
            };
        }

        // The stack grows down, so a caller sits above its callee; anything else is a lost walk.
        if context.Rsp <= callee_sp {
            break;
        }
    }

    count
}

/// The stack the frame at `index` uses: the distance to its caller's stack pointer.
///
/// The last frame in `frames` has no caller there, so it reports 0.
pub fn frame_size(frames: &[Frame], index: usize) -> usize {
    match (frames.get(index), frames.get(index + 1)) {
        (Some(frame), Some(caller)) => caller.sp.saturating_sub(frame.sp),
        _ => 0,
    }
}
