//! A handle-keyed map whose lock is **sharded** across many `RwLock`s.
//!
//! Every per-handle map the layer keeps (the [`ManagedRegistry`](super::ManagedRegistry)s for files
//! and DNS queries) wants the same thing. Many threads insert, look up, and remove concurrently.
//! One global lock must not serialize them.
//!
//! A single `RwLock<HashMap>` did exactly that. Under bursty load (a `.NET` `HttpClient` firing
//! many requests) it contended badly enough that the non-blocking `try_*` gave up and **dropped the
//! operation**. A DNS query surfaced as `No such host is known`; a file binding was lost or leaked.
//!
//! This is that map, factored out. The sharding, the bounded-spin retry, the contention logs, and
//! the [blocking insert](ShardedMap::insert) all live here in one place. An entry lives in the
//! shard chosen by `hash(key) & (SHARD_COUNT - 1)`, so unrelated handles take different locks.
//!
//! ## Observability
//!
//! The map narrates its own contention so the lock behaviour is never a black box:
//! - `debug` once an op has to spin past [`CONTENTION_LOG_AT`] (early warning, well before it
//!   bites);
//! - `warn` when [`insert`](ShardedMap::insert) exhausts its spin budget and has to *block*, or
//!   recovers from a poisoned shard lock (a writer panicked);
//! - `error` when [`get`](ShardedMap::get) / [`remove`](ShardedMap::remove) exhaust theirs and bail
//!   (the only paths that can still drop an op). Each line carries the map `label` and `spins`.

use std::{
    borrow::Borrow,
    collections::{HashMap, hash_map::RandomState},
    hash::{BuildHasher, Hash},
    sync::{RwLock, TryLockError},
};

/// Number of independent lock shards. Power of two so `& (N - 1)` selects a shard. Sized well above
/// the layer's worker/`task_pool` concurrency so two unrelated handles almost never contend.
const SHARD_COUNT: usize = 32;

/// Bounded spin on a momentarily-contended shard lock before giving up. With [`SHARD_COUNT`] shards
/// a same-shard collision is rare and short-lived, so this almost always wins on the first try.
const SPIN: usize = 128;

/// If an op spins at least this many times before winning the lock, log it at `debug` -- a window
/// into contention building up *before* it reaches [`SPIN`] (where `insert` blocks and `get` /
/// `remove` bail). Quiet in the common case: most ops win on the first try.
const CONTENTION_LOG_AT: usize = 16;

type Shard<K, V> = RwLock<HashMap<K, V>>;

/// A `HashMap<K, V>` split across [`SHARD_COUNT`] independent `RwLock`s.
///
/// `V` is cloned out of the map by [`get`](Self::get) (callers store cheap-to-clone values --
/// typically an `Arc`), so the shard lock is only ever held for the map operation itself, never
/// while the caller works with the value.
pub(crate) struct ShardedMap<K, V> {
    shards: Box<[Shard<K, V>]>,
    /// Fixed hasher so a key and any `Borrow<Q>` of it select the same shard.
    hasher: RandomState,
    /// Human-readable name for the contention logs (e.g. `"MANAGED_FILES"`).
    label: &'static str,
}

impl<K: Hash + Eq, V: Clone> ShardedMap<K, V> {
    /// An empty map labelled `label` (surfaces in the contention logs).
    pub(crate) fn new(label: &'static str) -> Self {
        let shards = std::iter::repeat_with(|| RwLock::new(HashMap::new()))
            .take(SHARD_COUNT)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            shards,
            hasher: RandomState::new(),
            label,
        }
    }

    /// The shard a key (or any `Borrow` of it) maps to.
    fn shard<Q>(&self, key: &Q) -> &Shard<K, V>
    where
        Q: Hash + ?Sized,
    {
        // `& (len - 1)` with a power-of-two `SHARD_COUNT` keeps `idx` in range;
        // `get` (over `[]`) keeps the no-slice-index-panic lint happy.
        let idx = self.hasher.hash_one(key) as usize & (self.shards.len() - 1);
        self.shards
            .get(idx)
            .expect("shard index is masked into range")
    }

    /// Emit a `debug` line if an op had to spin past [`CONTENTION_LOG_AT`] before acquiring -- the
    /// early-warning signal for shard contention, well below the point where it bails or blocks.
    fn note_contention(&self, op: &str, spins: usize) {
        if spins >= CONTENTION_LOG_AT {
            tracing::debug!(
                map = self.label,
                op,
                spins,
                "ShardedMap: shard lock contended; acquired after spinning"
            );
        }
    }

    /// Look up `key` and clone out its value.
    ///
    /// A momentarily-contended shard is retried briefly, so a transient conflict can't make a live
    /// entry vanish.
    ///
    /// # Returns
    ///
    /// The value, or `None` if `key` is absent.
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let shard = self.shard(key);
        for attempt in 0..SPIN {
            match shard.try_read() {
                Ok(map) => {
                    self.note_contention("get", attempt);
                    return map.get(key).cloned();
                }
                Err(TryLockError::Poisoned(p)) => return p.into_inner().get(key).cloned(),
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
        tracing::error!(
            map = self.label,
            spins = SPIN,
            "ShardedMap::get: shard read-lock stayed contended through the spin budget; reporting \
             the key as absent -- a live managed handle may now wrongly fall back to the original \
             syscall"
        );
        None
    }

    /// Insert `value` under `key`. **Always succeeds.**
    ///
    /// A managed handle the caller is about to hand out *must* be tracked. So unlike
    /// [`get`](Self::get) / [`remove`](Self::remove) (which stay non-blocking and bail), this one
    /// never drops the registration.
    ///
    /// # Blocking
    ///
    /// ⚠️ It spins ([`SPIN`] times) on a contended shard. Only if that is exhausted does it
    /// **block** on the shard write lock. That's been seen ~1 in 10,000 ops at 100-way
    /// concurrency.
    ///
    /// The wait is sub-millisecond and in-memory. The shard critical section is a bare `HashMap` op
    /// that holds no other lock and never the loader lock, so the holder always releases promptly.
    /// And `insert` is never re-entrant from inside [`for_each`](Self::for_each), so there is no
    /// read-then-write self-deadlock.
    ///
    /// It never waits on I/O, the agent, or the network. Callers invoke it on the app's own syscall
    /// thread, off the async-completion path.
    pub(crate) fn insert(&self, key: K, value: V) {
        let shard = self.shard(&key);
        for attempt in 0..SPIN {
            match shard.try_write() {
                Ok(mut map) => {
                    self.note_contention("insert", attempt);
                    map.insert(key, value);
                    return;
                }
                Err(TryLockError::Poisoned(p)) => {
                    // A writer panicked while holding this shard's lock. The map itself is intact,
                    // so we take it and complete the insert -- but a panic under a layer lock is an
                    // anomaly worth surfacing, and an insert must never be dropped silently.
                    tracing::warn!(
                        map = self.label,
                        "ShardedMap::insert: recovered from a poisoned shard lock (a writer \
                         panicked while holding it); registration kept"
                    );
                    p.into_inner().insert(key, value);
                    return;
                }
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
        // The spin budget didn't win. Block to acquire rather than drop a registration the caller
        // depends on -- loud, because reaching here means genuinely pathological contention.
        tracing::warn!(
            map = self.label,
            spins = SPIN,
            "ShardedMap::insert: shard write-lock contended past the spin budget; blocking to acquire \
             (registration kept, not dropped)"
        );
        shard
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, value);
    }

    /// Remove `key`.
    ///
    /// # Returns
    ///
    /// The removed value, or `None` if `key` was absent.
    pub(crate) fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let shard = self.shard(key);
        for attempt in 0..SPIN {
            match shard.try_write() {
                Ok(mut map) => {
                    self.note_contention("remove", attempt);
                    return map.remove(key);
                }
                Err(TryLockError::Poisoned(p)) => return p.into_inner().remove(key),
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
        tracing::error!(
            map = self.label,
            spins = SPIN,
            "ShardedMap::remove: shard write-lock stayed contended through the spin budget; entry \
             not removed (leaked)"
        );
        None
    }

    /// Visit every live entry with `f`.
    ///
    /// Reads one shard at a time, so it never freezes the whole map. A shard that stays contended
    /// through the spin budget is skipped (loudly) rather than blocked on.
    pub(crate) fn for_each(&self, mut f: impl FnMut(&K, &V)) {
        for (idx, shard) in self.shards.iter().enumerate() {
            let mut visited = false;
            for attempt in 0..SPIN {
                match shard.try_read() {
                    Ok(map) => {
                        self.note_contention("for_each", attempt);
                        for (k, v) in map.iter() {
                            f(k, v);
                        }
                        visited = true;
                        break;
                    }
                    Err(TryLockError::Poisoned(p)) => {
                        for (k, v) in p.into_inner().iter() {
                            f(k, v);
                        }
                        visited = true;
                        break;
                    }
                    Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
                }
            }
            if !visited {
                tracing::error!(
                    map = self.label,
                    shard = idx,
                    spins = SPIN,
                    "ShardedMap::for_each: shard stayed read-contended; its entries skipped this pass"
                );
            }
        }
    }
}
