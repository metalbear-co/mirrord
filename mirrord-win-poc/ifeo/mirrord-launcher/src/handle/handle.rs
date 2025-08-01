use winapi::um::{
    handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
    winnt::HANDLE,
};

/// Safe [`HANDLE`] abstraction.
///
/// Acquires raw [`HANDLE`] handle, does [`CloseHandle`] on [`std::ops::Drop`].
#[derive(Debug)]
pub struct SafeHandle {
    handle: HANDLE,
}

impl SafeHandle {
    /// Constructs [`SafeHandle`] from [`HANDLE`].
    ///
    /// # Arguments
    ///
    /// * `handle`: Windows raw [`HANDLE`] handle.
    pub fn from(handle: HANDLE) -> Self {
        Self { handle }
    }

    /// Get underlying raw [`HANDLE`] for Windows API operations.
    pub fn get(&self) -> HANDLE {
        self.handle
    }

    /// Close underlying raw [`HANDLE`] handle.
    pub fn close(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.get()) };
        }

        self.handle = std::ptr::null_mut();
    }
}

impl std::ops::Drop for SafeHandle {
    fn drop(&mut self) {
        self.close()
    }
}
