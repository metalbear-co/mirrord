//! Per-subsystem registry of managed handles.
//!
//! A [`ShardedMap`] keyed by a handle, plus a counter that mints fresh handle values. The sharding,
//! locking, and contention-logging all live in [`ShardedMap`]. This type just adds two things: the
//! handle allocation, and the `Arc<RwLock<V>>` per-entry value.
//!
//! That `Arc<RwLock<V>>` is what lets a hook clone the `Arc` out of the map, drop the shard lock,
//! and take the inner lock at its leisure. So there is no lock-order coupling between "look up by
//! handle" and "mutate context".

use std::{
    borrow::Borrow,
    hash::Hash,
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{
    handle::{CounterAllocated, ManagedHandleKey},
    sharded_map::ShardedMap,
};

/// A registry of managed handles. Singletons typically wrap this in [`once_cell::sync::Lazy`]:
///
/// ```ignore
/// pub static MANAGED_FILES: Lazy<ManagedRegistry<MirrordFileHandle, HandleContext>> =
///     Lazy::new(|| ManagedRegistry::new("MANAGED_FILES"));
/// ```
pub(crate) struct ManagedRegistry<K: ManagedHandleKey, V> {
    map: ShardedMap<K, Arc<RwLock<V>>>,
    counter: AtomicUsize,
}

impl<K: ManagedHandleKey, V> ManagedRegistry<K, V> {
    /// Look up a handle.
    ///
    /// # Returns
    ///
    /// A clone of its per-entry `Arc<RwLock<V>>`, or `None` if absent.
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<Arc<RwLock<V>>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(key)
    }

    /// Remove a handle.
    ///
    /// # Returns
    ///
    /// Its entry, or `None` if it was absent.
    pub(crate) fn remove<Q>(&self, key: &Q) -> Option<Arc<RwLock<V>>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.remove(key)
    }

    /// Visit every live entry (used by path-scan hooks that match across all handles).
    pub(crate) fn for_each(&self, f: impl FnMut(&K, &Arc<RwLock<V>>)) {
        self.map.for_each(f);
    }
}

impl<K: CounterAllocated, V> ManagedRegistry<K, V> {
    /// An empty registry whose first handed-out handle will be `K::FIRST`. `label` names the
    /// backing map in the contention logs.
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            map: ShardedMap::new(label),
            counter: AtomicUsize::new(K::FIRST),
        }
    }

    /// Insert `value` under a freshly-allocated handle. **Always succeeds.**
    ///
    /// # Blocking
    ///
    /// ⚠️ Delegates to [`ShardedMap::insert`](super::sharded_map::ShardedMap::insert), which under
    /// pathological shard contention blocks (briefly, in-memory, off any I/O) rather than dropping
    /// a registration the caller is about to hand out. See that method for the full safety
    /// argument.
    ///
    /// # Returns
    ///
    /// The freshly-allocated handle that `value` was stored under.
    pub(crate) fn insert(&self, value: V) -> K {
        let raw = self.counter.fetch_add(1, Ordering::Relaxed);
        let key = K::from_raw(raw);
        self.map.insert(key, Arc::new(RwLock::new(value)));
        key
    }
}
