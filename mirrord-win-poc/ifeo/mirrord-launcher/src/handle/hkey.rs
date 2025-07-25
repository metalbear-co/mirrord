use winapi::{shared::minwindef::HKEY, um::winreg::RegCloseKey};

/// Safe [`HKEY`] abstraction.
///
/// Acquires raw [`HKEY`] handle, does [`RegCloseKey`] on [`std::ops::Drop`].
#[derive(Debug)]
pub struct SafeHKey {
    handle: HKEY,
}

impl SafeHKey {
    /// Constructs [`SafeHKey`] from [`HKEY`].
    ///
    /// # Arguments
    ///
    /// * `handle`: Windows raw [`HKEY`] handle.
    pub fn from(handle: HKEY) -> Self {
        Self { handle }
    }

    /// Get underlying raw [`HKEY`] for Windows API operations.
    pub fn get(&self) -> HKEY {
        self.handle
    }

    /// Close underlying raw [`HKEY`] handle.
    pub fn close(&mut self) {
        if !self.handle.is_null() {
            unsafe { RegCloseKey(self.get()) };
            self.handle = std::ptr::null_mut();
        }
    }
}

impl std::ops::Drop for SafeHKey {
    fn drop(&mut self) {
        self.close()
    }
}
