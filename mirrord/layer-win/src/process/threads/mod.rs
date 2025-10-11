use phnt::ffi::{NtGetNextThread, NtResumeThread, NtSuspendThread};
use winapi::{
    shared::ntdef::NT_SUCCESS,
    um::{
        handleapi::CloseHandle,
        processthreadsapi::{GetCurrentProcess, GetCurrentThread, GetThreadId},
        winnt::{THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME},
    },
};

/// The possible states for a thread.
pub enum ThreadState {
    /// When setting state to `Frozen`, the `suspended` value of the thread is increased by 1.
    Frozen,
    /// When setting state to `Unfrozen`, the `suspended` value of the thread is decreased by 1.
    Unfrozen,
}

/// Change the state of all *but current* threads to `state`.
///
/// # Arguments
///
/// * `state` - A [`ThreadState`].
pub fn change_all_state(state: ThreadState) {
    unsafe {
        use phnt::ffi::HANDLE;

        let process = GetCurrentProcess() as HANDLE;
        let current_thread = GetCurrentThread() as HANDLE;
        let current_tid = GetThreadId(current_thread as _);

        let mut thread: HANDLE = std::ptr::null_mut();
        loop {
            let mut next_thread: HANDLE = std::ptr::null_mut();
            let status = NtGetNextThread(
                process,
                thread,
                THREAD_SUSPEND_RESUME | THREAD_QUERY_INFORMATION,
                0,
                0,
                &mut next_thread,
            );

            if !thread.is_null() {
                CloseHandle(thread as _);
            }

            if !NT_SUCCESS(status) {
                break;
            }

            if GetThreadId(next_thread as _) != current_tid {
                match state {
                    ThreadState::Frozen => {
                        NtSuspendThread(next_thread, std::ptr::null_mut());
                    }
                    ThreadState::Unfrozen => {
                        let mut count = 0u32;

                        // Make sure thread is *actually* resumed (suspend count is 0)
                        loop {
                            let status = NtResumeThread(next_thread, &mut count);
                            if count == 0 || !NT_SUCCESS(status) {
                                break;
                            }
                        }
                    }
                }
            }

            thread = next_thread;
        }
    }
}
