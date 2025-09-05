//! Utilities of the managed handle system.

use std::{cell::LazyCell, collections::HashMap, sync::RwLock};

use winapi::{
    shared::{
        minwindef::{FILETIME, ULONG},
        ntdef::HANDLE,
    },
    um::winnt::ACCESS_MASK,
};

/// This is a [`HANDLE`] type. The values start with [`MIRRORD_FIRST_MANAGED_HANDLE`].
/// To know what data is held behind this, look at [`HandleContext`].
pub type MirrordHandle = HANDLE;

/// This is the minimum possible value of a [`MirrordHandle`].
pub const MIRRORD_FIRST_MANAGED_HANDLE: MirrordHandle = 0x50000000 as _;

/// This is a [`RwLock`]-guarded [`HashMap`] of [`MirrordHandle`]'s that map to [`HandleContext`]'s.
pub static mut MANAGED_HANDLES: RwLock<LazyCell<HashMap<MirrordHandle, HandleContext>>> =
    RwLock::new(LazyCell::new(|| HashMap::new()));

/// The data behind a [`MirrordHandle`].
pub struct HandleContext {
    /// Remote file descriptor for file
    pub fd: u64,
    /// Windows desired access
    pub desired_access: ACCESS_MASK,
    /// Windows file attributes
    pub file_attributes: ULONG,
    /// Windows share access
    pub share_access: ULONG,
    /// Windows create disposition
    pub create_disposition: ULONG,
    /// Windows create options
    pub create_options: ULONG,
    /// The Linux path, maps to the `fd`
    pub path: String,
    /// Creation time as [`FILETIME`]
    pub creation_time: FILETIME,
}

/// Try to linearly insert a new [`MirrordHandle`] starting at [`MIRRORD_FIRST_MANAGED_HANDLE`].
///
/// # Arguments
///
/// * `handle_context`: The first state of the context behind the handle.
///
/// # Return value
///
/// * `Some(MirrordHandle)` if the operation succeeded
/// * `None` if the operation failed
pub fn try_insert_handle(handle_context: HandleContext) -> Option<MirrordHandle> {
    if let Ok(mut handles) = unsafe { MANAGED_HANDLES.try_write() } {
        let new_handle = handles
            .keys()
            .last()
            .map(|x| (*x as usize + 1) as _)
            .unwrap_or(MIRRORD_FIRST_MANAGED_HANDLE);
        handles.insert(new_handle, handle_context);

        Some(new_handle)
    } else {
        None
    }
}
