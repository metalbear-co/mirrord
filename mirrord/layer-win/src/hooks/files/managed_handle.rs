//! Utilities of the managed handle system.

use std::{
    borrow::Borrow,
    collections::HashMap,
    hash::Hash,
    ops::Deref,
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use once_cell::sync::Lazy;
use winapi::{
    shared::{
        minwindef::{FILETIME, ULONG},
        ntdef::HANDLE,
    },
    um::winnt::ACCESS_MASK,
};

/// This is a [`HANDLE`] type. The values start with [`MIRRORD_FIRST_MANAGED_HANDLE`].
/// To know what data is held behind this, look at [`HandleContext`].
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirrordHandle(pub HANDLE);

unsafe impl Send for MirrordHandle {}
unsafe impl Sync for MirrordHandle {}

/// Implements support for `map.get(&HANDLE)`
impl Borrow<HANDLE> for MirrordHandle {
    fn borrow(&self) -> &HANDLE {
        &self.0
    }
}

impl Deref for MirrordHandle {
    type Target = HANDLE;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// This is the minimum possible value of a [`MirrordHandle`].
pub const MIRRORD_FIRST_MANAGED_HANDLE: MirrordHandle = MirrordHandle(0x50000000 as _);

/// Map [`MirrordHandle`] to [`HandleContext`].
pub static MANAGED_HANDLES: Lazy<RwLock<HashMap<MirrordHandle, Arc<RwLock<HandleContext>>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Keeps track of latest handle.
static HANDLE_COUNTER: AtomicUsize =
    AtomicUsize::new(unsafe { std::mem::transmute(MIRRORD_FIRST_MANAGED_HANDLE.0) });

/// The data behind a [`MirrordHandle`].
pub struct HandleContext {
    /// The Linux path, maps to the `fd`
    pub path: String,
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
    /// Creation time as [`FILETIME`]
    pub creation_time: FILETIME,
    /// Access time as [`FILETIME`]
    pub access_time: FILETIME,
    /// Write time as [`FILETIME`]
    pub write_time: FILETIME,
    /// Change time as [`FILETIME`]
    pub change_time: FILETIME,
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
    if let Ok(mut handles) = MANAGED_HANDLES.try_write() {
        let new_handle_val = HANDLE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let new_handle = MirrordHandle(new_handle_val as _);
        handles.insert(new_handle, Arc::new(RwLock::new(handle_context)));

        Some(new_handle)
    } else {
        None
    }
}
