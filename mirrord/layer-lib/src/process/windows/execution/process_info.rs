//! RAII wrapper for Windows process information.
//!
//! This module provides automatic handle management for Windows processes,
//! ensuring proper cleanup in both success and error scenarios.

use winapi::um::{
    processthreadsapi::{PROCESS_INFORMATION, TerminateProcess},
    handleapi::CloseHandle,
};

/// RAII wrapper for Windows process information that automatically cleans up handles.
/// 
/// This provides automatic handle cleanup through Drop, with the ability to release
/// handles when the process should continue running normally.
/// 
/// # Examples
/// 
/// ## Successful process creation:
/// ```rust,ignore
/// let raw_process_info = create_process()?;
/// let managed = ManagedProcessInfo::new(raw_process_info);
/// 
/// // Do setup work (DLL injection, etc.)
/// setup_process(&managed)?;
/// 
/// // Release on success - process continues running
/// let process_info = managed.release();
/// ```
/// 
/// ## Error handling:
/// ```rust,ignore
/// let raw_process_info = create_process()?;
/// let managed = ManagedProcessInfo::new(raw_process_info);
/// 
/// if let Err(e) = setup_process(&managed) {
///     // Terminate and cleanup automatically
///     managed.terminate_and_cleanup(1);
///     return Err(e);
/// }
/// // Or handles are automatically cleaned up on Drop if not released
/// ```
pub struct ManagedProcessInfo {
    process_info: PROCESS_INFORMATION,
    released: bool,
}

impl ManagedProcessInfo {
    /// Create a new managed process info wrapper.
    /// 
    /// Takes ownership of the process information and will manage the handles
    /// until either `release()` or `terminate_and_cleanup()` is called.
    pub fn new(process_info: PROCESS_INFORMATION) -> Self {
        Self {
            process_info,
            released: false,
        }
    }

    /// Get a reference to the underlying process information.
    /// 
    /// This allows read-only access to the process info without transferring ownership.
    pub fn as_ref(&self) -> &PROCESS_INFORMATION {
        &self.process_info
    }

    /// Release the handles without cleanup, indicating the process should continue normally.
    /// 
    /// This is called when the process creation succeeds and we want the process to
    /// continue running. The caller becomes responsible for handle management.
    /// 
    /// # Returns
    /// 
    /// The underlying `PROCESS_INFORMATION` structure with handles transferred to the caller.
    pub fn release(mut self) -> PROCESS_INFORMATION {
        self.released = true;
        self.process_info
    }

    /// Terminate the process and clean up handles.
    /// 
    /// This is used when we need to forcibly terminate a process due to an error
    /// during setup (e.g., DLL injection failure). The process is terminated with
    /// the specified exit code and all handles are closed.
    /// 
    /// # Arguments
    /// 
    /// * `exit_code` - The exit code to use when terminating the process
    pub fn terminate_and_cleanup(mut self, exit_code: u32) {
        tracing::debug!(
            pid = self.process_info.dwProcessId,
            exit_code = exit_code,
            "terminating process and cleaning up handles"
        );
        
        unsafe {
            TerminateProcess(self.process_info.hProcess, exit_code);
            CloseHandle(self.process_info.hProcess);
            CloseHandle(self.process_info.hThread);
        }
        self.released = true; // Mark as released to avoid double cleanup
    }
}

impl Drop for ManagedProcessInfo {
    /// Automatically clean up handles if not explicitly released.
    /// 
    /// This ensures we don't leak handles if an error occurs before the process
    /// is successfully set up and released. Note that this does NOT terminate
    /// the process - it only closes the handles in case the process is still running.
    fn drop(&mut self) {
        if !self.released {
            tracing::debug!(
                pid = self.process_info.dwProcessId,
                "auto-cleaning up unreleased process handles (process may still be running)"
            );
            unsafe {
                // Don't terminate - just close handles in case process is still running
                CloseHandle(self.process_info.hProcess);
                CloseHandle(self.process_info.hThread);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    /// Create a dummy PROCESS_INFORMATION for testing
    fn dummy_process_info() -> PROCESS_INFORMATION {
        unsafe { mem::zeroed() }
    }

    #[test]
    fn test_managed_process_info_new() {
        let info = dummy_process_info();
        let managed = ManagedProcessInfo::new(info);
        assert!(!managed.released);
    }

    #[test]
    fn test_managed_process_info_release() {
        let info = dummy_process_info();
        let managed = ManagedProcessInfo::new(info);
        let _released_info = managed.release();
        // Should not panic or leak
    }

    #[test]
    fn test_managed_process_info_terminate_and_cleanup() {
        let info = dummy_process_info();
        let managed = ManagedProcessInfo::new(info);
        managed.terminate_and_cleanup(1);
        // Should not panic or leak
    }
}