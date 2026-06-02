//! The HashMap-of-Arc-of-RwLock pattern that used to live by hand in
//! `hooks/files/managed_handle.rs`, lifted to be reusable across the
//! handle kinds the layer manages.

use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock, TryLockError, TryLockResult,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::handle::{CounterAllocated, ManagedHandleKey};

/// Per-subsystem registry of managed handles. Each registry owns:
///
/// 1. A `RwLock<HashMap<K, Arc<RwLock<V>>>>` for the actual handle->context mapping. The two-level
///    locking is deliberate -- the outer lock guards the *map structure* (which handles exist), the
///    inner lock guards a *single context*. Decoupling them gives callers four meaningful lock-pair
///    combinations:
///
///    | outer | inner | what this enables                                  |
///    |-------|-------|----------------------------------------------------|
///    | read  | read  | concurrent lookups + concurrent context reads      |
///    | read  | write | **mutate one entry's context without freezing the map** -- the common hot path for an async IO completing on handle X while handle Y is being looked up by some other thread |
///    | write | read  | **add/remove handles while reading targeted entries** -- e.g. derive a new entry's state from an existing one during insert |
///    | write | write | exclusive across map + entry (used only by combined remove-and-finalize sequences) |
///
///    The `Arc<RwLock<V>>` per entry means a hook can drop the outer read
///    lock immediately after cloning the Arc, then take the inner write
///    lock at its leisure -- no lock-order coupling between "look up by
///    handle" and "mutate context."
///
/// 2. An `AtomicUsize` counter producing fresh handle values, starting at `K::FIRST`.
///
/// Singletons typically wrap this in [`once_cell::sync::Lazy`]:
///
/// ```ignore
/// pub static MANAGED_FILES: Lazy<ManagedRegistry<MirrordFileHandle, HandleContext>> =
///     Lazy::new(ManagedRegistry::new);
/// ```
pub(crate) struct ManagedRegistry<K: ManagedHandleKey, V> {
    inner: RwLock<HashMap<K, Arc<RwLock<V>>>>,
    counter: AtomicUsize,
}

impl<K: ManagedHandleKey, V> ManagedRegistry<K, V> {
    /// Try to acquire a read lock over the handle map. Returns the
    /// underlying `TryLockResult` unchanged so callers can ignore poison
    /// vs. contention as they see fit (existing hooks use a bare
    /// `if let Ok(...)`).
    pub(crate) fn try_read(
        &self,
    ) -> TryLockResult<std::sync::RwLockReadGuard<'_, HashMap<K, Arc<RwLock<V>>>>> {
        self.inner.try_read()
    }

    /// Counterpart to [`Self::try_read`]; used by inserts/removes.
    pub(crate) fn try_write(
        &self,
    ) -> TryLockResult<std::sync::RwLockWriteGuard<'_, HashMap<K, Arc<RwLock<V>>>>> {
        self.inner.try_write()
    }
}

impl<K: CounterAllocated, V> ManagedRegistry<K, V> {
    /// Construct an empty registry whose first handed-out handle will be
    /// `K::FIRST`.
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            counter: AtomicUsize::new(K::FIRST),
        }
    }

    /// Insert `value` under a freshly-allocated handle, returning the new
    /// key. Returns `None` if the outer write lock is contended; callers
    /// in hook context are expected to treat this as "fall back to the
    /// original syscall" rather than blocking.
    pub(crate) fn try_insert(&self, value: V) -> Option<K> {
        match self.inner.try_write() {
            Ok(mut handles) => {
                let raw = self.counter.fetch_add(1, Ordering::Relaxed);
                let key = K::from_raw(raw);
                handles.insert(key, Arc::new(RwLock::new(value)));
                Some(key)
            }
            Err(TryLockError::Poisoned(_)) | Err(TryLockError::WouldBlock) => None,
        }
    }
}

impl<K: CounterAllocated, V> Default for ManagedRegistry<K, V> {
    fn default() -> Self {
        Self::new()
    }
}
