//! Macros module for process operations.

/// Macro that waits for debugger to become present.
///
/// This is an expensive operation. To not completely waste time, the
/// thread time slice is reallocated to other threads by using `Sleep(0)`.
#[macro_export]
macro_rules! wait_for_debug {
    () => {{
        unsafe {
            use winapi::shared::ntdef::FALSE;
            while winapi::um::debugapi::IsDebuggerPresent() == FALSE as i32 {
                // must busy-wait for the debugger and not sleep
                // winapi::um::synchapi::Sleep(0);
            }
        }
    }};
}
