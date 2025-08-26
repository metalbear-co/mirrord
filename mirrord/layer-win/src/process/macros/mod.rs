//! Macros module for process operations.

/// Macro that waits for debugger to become present.
///
/// This is an expensive operation. To not completely waste time, the
/// thread time slice is reallocated to other threads by using `Sleep(0)`.
#[macro_export]
macro_rules! wait_for_debug {
    () => {
        (|| -> bool {
            loop {
                unsafe {
                    if winapi::um::debugapi::IsDebuggerPresent() == 0 {
                        winapi::um::synchapi::Sleep(0); 
                    } else {
                        return true;
                    }
                }
            }

            #[allow(unreachable_code)]
            false
        })()
    };
}