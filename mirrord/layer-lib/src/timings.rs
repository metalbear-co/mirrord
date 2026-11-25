//! Always-on tally of the time the application spends waiting on mirrord.
//!
//! Every call the layer answers from the cluster is one request to the internal proxy and one
//! wait for its answer. [`CallTimer`] measures that wait and adds it to a fixed set of counters,
//! one per [`CallKind`]. The counters are sent to the internal proxy as a
//! [`CallTimingsSnapshot`], which is what `mirrord diagnose timings` prints.
//!
//! This runs inside the user's application on every remote call, in release builds too, so
//! measuring a call costs two clock reads and a few atomic updates: no allocation, no lock, no
//! log line. Two things do allocate and take a lock, and both are rare: building a snapshot,
//! which happens at most once per [`SNAPSHOT_INTERVAL`] once a process is a few seconds old, and
//! remembering what a call was for, which happens only for a call slow enough to enter the list
//! of the slowest ones (see [`SLOW_CALL_THRESHOLD`]).
//!
//! A snapshot never gets a send of its own. It goes out in front of the next request the
//! application makes (see [`take_ready_snapshot`]), so counting adds no moment at which a thread
//! of the application waits on the connection for something the application did not ask for.
//! The calls of a process that goes quiet or dies are still in the summary: the internal proxy
//! counts every call itself.

use std::{
    sync::{
        Mutex, MutexGuard, OnceLock, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use mirrord_intproxy_protocol::{
    CallKind, CallTimingsSnapshot, KindTimings, LayerToProxyMessage, MAX_SLOW_CALLS, SlowCall,
};

/// The longest a process goes between two snapshots while it keeps making remote calls.
///
/// The application can be killed at any moment, and whatever it measured since its last snapshot
/// is lost with it, so this is the most a long-running process can lose. The calls themselves
/// are not lost: the internal proxy counts them too and reports its own, slightly shorter,
/// measurement for whatever no snapshot covered. A snapshot is one small
/// write to a local socket, so sending one every two seconds costs nothing next to the remote
/// calls it travels with.
///
/// No timer runs: the first remote call that finishes once a snapshot is due marks it ready, and
/// the next request carries it, so the layer starts no thread of its own in the application.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(2);

/// How long a process counts before its first snapshot.
///
/// A young process sends snapshots more often than [`SNAPSHOT_INTERVAL`]: the first one after
/// this long, and each next one after as long again as the process had been counting at the
/// previous one, so the gaps double until they reach the interval. A process that dies without
/// warning then loses about the last half of its own measurements at most, however short its
/// run was. It costs a handful of extra snapshots per process.
const FIRST_SNAPSHOT_AFTER: Duration = Duration::from_millis(50);

/// The shortest wait that can put a call on the list of the slowest ones.
///
/// A tenth of a second is about where a person starts to notice a single call. Against a cluster
/// close to the user most calls take a few milliseconds and never get here, so they pay nothing
/// for the list. Against a far cluster every call can be this slow, which is why a full list
/// raises the bar to its own fastest entry (see [`SLOW_CALL_FLOOR_NANOS`]): after the first few
/// calls only a call slower than everything remembered so far does the extra work.
const SLOW_CALL_THRESHOLD: Duration = Duration::from_millis(100);

/// Off until the layer turns it on, so a layer that never does (the Windows layer, a layer with
/// `experimental.call_timings` off) pays one atomic read per remote call and nothing else.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// The moment counting was turned on. Snapshot times are stored as nanoseconds since this moment
/// because an [`Instant`] does not fit in an atomic.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// When this process started counting, in nanoseconds since [`ORIGIN`]. Zero except in a forked
/// child, which starts over at the fork.
static STARTED_NANOS: AtomicU64 = AtomicU64::new(0);

/// When the last snapshot was claimed, in nanoseconds since [`ORIGIN`]. Before the first
/// snapshot, when this process started counting.
static LAST_SNAPSHOT_NANOS: AtomicU64 = AtomicU64::new(0);

/// Set when a snapshot is due, until a request takes it along.
static SNAPSHOT_READY: AtomicBool = AtomicBool::new(false);

static TALLIES: [KindTally; CallKind::ALL.len()] =
    [const { KindTally::new() }; CallKind::ALL.len()];

/// The slowest calls of this process, with what they were for.
///
/// Only ever taken with `try_lock`: a thread that finds it held skips its entry, because the
/// application's call must never wait for a diagnostic list. A child forked while another thread
/// held it never gets it back and reports counters only.
static SLOWEST: Mutex<SlowestCalls> = Mutex::new(SlowestCalls::new());

/// The wait a call has to exceed to enter [`SLOWEST`]: [`SLOW_CALL_THRESHOLD`] until the list is
/// full, the fastest entry's wait after that. Read on every call so the common, faster call
/// never touches the lock.
static SLOW_CALL_FLOOR_NANOS: AtomicU64 = AtomicU64::new(SLOW_CALL_THRESHOLD.as_nanos() as u64);

/// At most [`MAX_SLOW_CALLS`] calls, the slowest seen so far.
struct SlowestCalls(Vec<SlowCall>);

impl SlowestCalls {
    const fn new() -> Self {
        Self(Vec::new())
    }

    /// Adds `call`, in place of the fastest entry when the list is full and `call` is slower.
    fn insert(&mut self, call: SlowCall) {
        if self.0.len() < MAX_SLOW_CALLS {
            self.0.push(call);
            return;
        }

        if let Some(fastest) = self.0.iter_mut().min_by_key(|kept| kept.nanos)
            && fastest.nanos < call.nanos
        {
            *fastest = call;
        }
    }

    /// The wait a call has to exceed to change this list.
    fn floor_nanos(&self) -> u64 {
        let threshold = saturating_nanos(SLOW_CALL_THRESHOLD);
        if self.0.len() < MAX_SLOW_CALLS {
            return threshold;
        }

        self.0
            .iter()
            .map(|kept| kept.nanos)
            .min()
            .map_or(threshold, |fastest| fastest.max(threshold))
    }
}

/// [`SLOWEST`] when no other thread holds it. The list is plain data that every writer leaves
/// valid, so a lock poisoned by a panic elsewhere is still used.
fn try_lock_slowest() -> Option<MutexGuard<'static, SlowestCalls>> {
    match SLOWEST.try_lock() {
        Ok(slowest) => Some(slowest),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

struct KindTally {
    calls: AtomicU64,
    total_nanos: AtomicU64,
    slowest_nanos: AtomicU64,
}

impl KindTally {
    const fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            total_nanos: AtomicU64::new(0),
            slowest_nanos: AtomicU64::new(0),
        }
    }

    fn add(&self, nanos: u64) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.total_nanos.fetch_add(nanos, Ordering::Relaxed);
        self.slowest_nanos.fetch_max(nanos, Ordering::Relaxed);
    }

    fn clear(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.total_nanos.store(0, Ordering::Relaxed);
        self.slowest_nanos.store(0, Ordering::Relaxed);
    }
}

/// Turns counting on for this process.
pub fn enable() {
    ORIGIN.get_or_init(Instant::now);
    ENABLED.store(true, Ordering::Relaxed);
}

/// Starts this process from zero.
///
/// A forked child inherits a copy of its parent's counters, and the parent keeps reporting those
/// calls itself. The child calls this so the same calls are not reported twice.
pub fn reset() {
    TALLIES.iter().for_each(KindTally::clear);
    if let Some(mut slowest) = try_lock_slowest() {
        slowest.0.clear();
        SLOW_CALL_FLOOR_NANOS.store(slowest.floor_nanos(), Ordering::Relaxed);
    }
    let now_nanos = nanos_since_origin(Instant::now());
    STARTED_NANOS.store(now_nanos, Ordering::Relaxed);
    LAST_SNAPSHOT_NANOS.store(now_nanos, Ordering::Relaxed);
    SNAPSHOT_READY.store(false, Ordering::Relaxed);
}

fn nanos_since_origin(now: Instant) -> u64 {
    let since_origin = ORIGIN
        .get()
        .map(|origin| now.saturating_duration_since(*origin))
        .unwrap_or_default();
    saturating_nanos(since_origin)
}

fn saturating_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Whether a snapshot is due at `now_nanos`, for a process that started counting at
/// `started_nanos` and claimed its last snapshot at `last_nanos`.
///
/// The gap to wait is how long the process had been counting at its last snapshot, kept between
/// [`FIRST_SNAPSHOT_AFTER`] and [`SNAPSHOT_INTERVAL`].
fn snapshot_due(started_nanos: u64, last_nanos: u64, now_nanos: u64) -> bool {
    let gap = last_nanos.saturating_sub(started_nanos).clamp(
        saturating_nanos(FIRST_SNAPSHOT_AFTER),
        saturating_nanos(SNAPSHOT_INTERVAL),
    );
    now_nanos.saturating_sub(last_nanos) >= gap
}

/// One remote call being measured, from just before its request is sent.
pub(crate) struct CallTimer {
    kind: CallKind,
    started: Instant,
}

impl CallTimer {
    /// Starts measuring the call behind `message`.
    ///
    /// # Returns
    ///
    /// [`None`] when counting is off or the message is not a call answered from the cluster.
    pub(crate) fn start(message: &LayerToProxyMessage) -> Option<Self> {
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }

        Some(Self {
            kind: CallKind::of(message)?,
            started: Instant::now(),
        })
    }

    /// Adds the finished call to its kind's counters, and to the slowest calls when it is slow
    /// enough. `message` is the request the timer was started with.
    ///
    /// When a snapshot is due this only marks it ready. The call was just answered, and its
    /// thread must go back to the application without touching the connection again.
    pub(crate) fn finish(self, message: &LayerToProxyMessage) {
        let now = Instant::now();
        let waited = saturating_nanos(now.saturating_duration_since(self.started));
        if let Some(tally) = TALLIES.get(self.kind as usize) {
            tally.add(waited);
        }
        if waited > SLOW_CALL_FLOOR_NANOS.load(Ordering::Relaxed)
            && let Some(mut slowest) = try_lock_slowest()
        {
            slowest.insert(SlowCall {
                kind: self.kind,
                subject: SlowCall::subject_of(message),
                nanos: waited,
            });
            SLOW_CALL_FLOOR_NANOS.store(slowest.floor_nanos(), Ordering::Relaxed);
        }

        let now_nanos = nanos_since_origin(now);
        let last_nanos = LAST_SNAPSHOT_NANOS.load(Ordering::Relaxed);
        if snapshot_due(STARTED_NANOS.load(Ordering::Relaxed), last_nanos, now_nanos)
            && LAST_SNAPSHOT_NANOS
                .compare_exchange(last_nanos, now_nanos, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            SNAPSHOT_READY.store(true, Ordering::Relaxed);
        }
    }
}

/// The snapshot to send in front of the request that is about to go out, when one is due.
///
/// Costs one atomic read for the requests that carry none, which is nearly all of them.
pub(crate) fn take_ready_snapshot() -> Option<CallTimingsSnapshot> {
    if !SNAPSHOT_READY.load(Ordering::Relaxed) || !SNAPSHOT_READY.swap(false, Ordering::Relaxed) {
        return None;
    }

    snapshot()
}

/// Gives a test the counters to itself, switched off and empty.
///
/// The counters are process-wide, so tests that touch them take turns.
#[cfg(test)]
pub(crate) fn exclusive_for_test() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());

    let turn = TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ENABLED.store(false, Ordering::Relaxed);
    reset();
    turn
}

/// The counters of every kind this process made at least one call of.
///
/// # Returns
///
/// [`None`] when counting is off or nothing was counted, so there is nothing worth sending.
fn snapshot() -> Option<CallTimingsSnapshot> {
    if !ENABLED.load(Ordering::Relaxed) {
        return None;
    }

    let kinds = CallKind::ALL
        .iter()
        .zip(&TALLIES)
        .filter_map(|(kind, tally)| {
            let calls = tally.calls.load(Ordering::Relaxed);
            (calls > 0).then(|| KindTimings {
                kind: *kind,
                calls,
                total_nanos: tally.total_nanos.load(Ordering::Relaxed),
                slowest_nanos: tally.slowest_nanos.load(Ordering::Relaxed),
            })
        })
        .collect::<Vec<_>>();

    if kinds.is_empty() {
        return None;
    }

    // A list another thread is changing right now is left out of this snapshot; the next one,
    // which replaces this one in the proxy, has it.
    let slowest = try_lock_slowest()
        .map(|slowest| slowest.0.clone())
        .unwrap_or_default();

    Some(CallTimingsSnapshot { kinds, slowest })
}

#[cfg(test)]
mod tests {
    use mirrord_protocol::{
        FileRequest,
        dns::{AddressFamily, GetAddrInfoRequestV2, SockType},
        file::{CloseFileRequest, XstatRequest},
    };

    use super::*;

    fn lookup() -> LayerToProxyMessage {
        LayerToProxyMessage::GetAddrInfo(GetAddrInfoRequestV2 {
            node: "db".to_owned(),
            service_port: 0,
            flags: 0,
            family: AddressFamily::Any,
            socktype: SockType::Any,
            protocol: 0,
        })
    }

    fn stat() -> LayerToProxyMessage {
        LayerToProxyMessage::File(FileRequest::Xstat(XstatRequest {
            path: Some("/app/settings.yaml".into()),
            fd: None,
            follow_symlink: true,
        }))
    }

    #[test]
    fn counts_remote_calls_per_kind_and_starts_over_after_a_fork() {
        let _turn = exclusive_for_test();

        assert!(
            CallTimer::start(&lookup()).is_none(),
            "nothing is measured until the layer turns counting on"
        );
        assert_eq!(snapshot(), None);

        enable();
        assert_eq!(snapshot(), None, "no calls yet, nothing worth sending");

        for message in [lookup(), stat(), stat()] {
            CallTimer::start(&message)
                .expect("a remote call is measured")
                .finish(&message);
        }
        let close = LayerToProxyMessage::File(FileRequest::Close(CloseFileRequest { fd: 1 }));
        assert!(
            CallTimer::start(&close).is_none(),
            "the layer does not wait for an answer to close, so it is not a measured call"
        );

        let counted = snapshot().expect("three calls were counted");
        assert_eq!(
            counted.slowest,
            [],
            "calls faster than the threshold are counted, not remembered one by one"
        );
        let calls = counted
            .kinds
            .into_iter()
            .map(|kind| (kind.kind, kind.calls))
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            [(CallKind::NameLookup, 1), (CallKind::FileStat, 2)],
            "one row per kind, in the order the kinds are declared"
        );

        reset();
        assert_eq!(
            snapshot(),
            None,
            "a forked child reports only its own calls"
        );
    }

    /// A finished call that finds a snapshot due only marks it, once, for a request to take
    /// along. A forked child starts without one.
    #[test]
    fn a_due_snapshot_waits_to_be_taken_and_is_taken_once() {
        let _turn = exclusive_for_test();
        enable();
        let message = stat();
        let call = || {
            CallTimer::start(&message)
                .expect("a remote call is measured")
                .finish(&message);
        };

        call();
        assert_eq!(
            take_ready_snapshot(),
            None,
            "a process younger than the first snapshot's delay has none ready"
        );

        std::thread::sleep(FIRST_SNAPSHOT_AFTER);
        call();
        let taken = take_ready_snapshot().expect("the call found the first snapshot due");
        assert_eq!(
            taken
                .kinds
                .iter()
                .map(|kind| kind.calls)
                .collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            take_ready_snapshot(),
            None,
            "one request carries it, not two"
        );

        std::thread::sleep(FIRST_SNAPSHOT_AFTER * 2);
        call();
        reset();
        assert_eq!(
            take_ready_snapshot(),
            None,
            "a snapshot that was ready in the parent holds the parent's calls"
        );
    }

    /// A process that is killed loses what it counted since its last snapshot. With a fixed
    /// two-second interval, a process killed in its first two seconds lost everything, which is
    /// what a real run on staging showed. Young processes therefore report early and often.
    #[test]
    fn a_young_process_sends_snapshots_at_doubling_gaps_up_to_the_interval() {
        const MS: u64 = 1_000_000;
        let interval = saturating_nanos(SNAPSHOT_INTERVAL);

        assert!(!snapshot_due(0, 0, 49 * MS));
        assert!(snapshot_due(0, 0, 50 * MS), "the first snapshot");
        assert!(!snapshot_due(0, 50 * MS, 99 * MS));
        assert!(snapshot_due(0, 50 * MS, 100 * MS), "as long again");
        assert!(!snapshot_due(0, 400 * MS, 799 * MS));
        assert!(snapshot_due(0, 400 * MS, 800 * MS));

        let hour = 3_600_000 * MS;
        assert!(!snapshot_due(0, hour, hour + interval - 1));
        assert!(
            snapshot_due(0, hour, hour + interval),
            "an old process never waits longer than the interval"
        );

        assert!(
            snapshot_due(hour, hour, hour + 50 * MS),
            "a forked child is young again, however old its parent was"
        );
        assert!(
            !snapshot_due(0, 1_000, 500),
            "a claim from another thread that is ahead of this call's clock read is not overdue"
        );
    }

    fn slow(subject: &str, nanos: u64) -> SlowCall {
        SlowCall {
            kind: CallKind::NameLookup,
            subject: Some(subject.to_owned()),
            nanos,
        }
    }

    /// The list answers "which calls were the slow ones", so when it is full a new call takes
    /// the place of the fastest entry, and only if it is slower. The bar for the next call is
    /// then the fastest entry, which is what keeps most calls away from the lock.
    #[test]
    fn a_full_list_keeps_the_slowest_calls_and_raises_the_bar() {
        let threshold = saturating_nanos(SLOW_CALL_THRESHOLD);
        let mut slowest = SlowestCalls::new();
        assert_eq!(slowest.floor_nanos(), threshold);

        for index in 0..MAX_SLOW_CALLS as u64 {
            slowest.insert(slow("early", threshold + 10 + index));
        }
        assert_eq!(slowest.floor_nanos(), threshold + 10, "the fastest entry");

        slowest.insert(slow("not slower than the fastest entry", threshold + 10));
        slowest.insert(slow("slower", threshold + 500));

        let mut kept = slowest
            .0
            .iter()
            .map(|call| (call.subject.as_deref().unwrap_or_default(), call.nanos))
            .collect::<Vec<_>>();
        kept.sort_by_key(|(_, nanos)| *nanos);
        assert_eq!(kept.len(), MAX_SLOW_CALLS);
        assert_eq!(kept.first(), Some(&("early", threshold + 11)));
        assert_eq!(kept.last(), Some(&("slower", threshold + 500)));
        assert_eq!(slowest.floor_nanos(), threshold + 11);
    }

    /// The counters are an array indexed by kind, so the index of every kind has to stay inside
    /// it and match the order `snapshot` walks the kinds in.
    #[test]
    fn every_kind_has_its_own_counter() {
        for (index, kind) in CallKind::ALL.iter().enumerate() {
            assert_eq!(*kind as usize, index);
        }
        assert_eq!(CallKind::ALL.len(), TALLIES.len());
    }
}
