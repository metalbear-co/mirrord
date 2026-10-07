//! Macros module for process operations.

/// Macro that waits for debugger to become present.
///
/// Polls between short sleeps, so a process left waiting for a debugger does not keep a core busy.
/// `Sleep` is a plain kernel call, safe under the loader lock this runs under.
#[macro_export]
macro_rules! wait_for_debug {
    () => {{
        unsafe {
            while winapi::um::debugapi::IsDebuggerPresent() == winapi::shared::minwindef::FALSE {
                winapi::um::synchapi::Sleep(50);
            }
        }
    }};
}
