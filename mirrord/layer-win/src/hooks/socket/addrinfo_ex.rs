//! Async state machine for the `GetAddrInfoExW` hook.
//!
//! `GetAddrInfoExW` can be called asynchronously: the caller passes an
//! `OVERLAPPED` and/or a `LPLOOKUPSERVICE_COMPLETION_ROUTINE`, the function
//! returns `WSA_IO_PENDING`, and the result is delivered *later* by invoking
//! the completion routine (or signaling the overlapped's event), with the
//! cancel handle (`lpNameHandle`) usable via `GetAddrInfoExCancel`. This is
//! NOT an IO-completion-port mechanism — there is no port to post to.
//!
//! mirrord's resolution is a blocking proxy round-trip, so we emulate the
//! async contract: stash a per-request [`AsyncQuery`], return `WSA_IO_PENDING`,
//! run the blocking resolve on the shared [`crate::task_pool`], and then fire
//! the completion exactly once.
//!
//! ## Exactly-once completion
//!
//! Two actors can complete a query: the worker (resolution finished) and a
//! `GetAddrInfoExCancel` call. They race on [`AsyncQuery::claim`], a single
//! atomic CAS — the winner delivers, the loser does nothing. The worker's
//! [`CompletionGuard`] additionally fires a failure on `Drop` if it never
//! delivered (worker panic / early return), so the caller — whose `Task` has
//! no timeout (`Dns.GetHostAddressesAsync` passes `timeout = NULL`) — can never
//! hang waiting for a completion that will not come.
//!
//! ## Cancel-handle strategy
//!
//! We never hand the caller a real ws2_32 handle. `lpNameHandle` receives a
//! synthetic handle minted by the layer's managed-handle registry
//! ([`crate::managed`]) in a range disjoint from OS handles; [`cancel`]
//! recognizes our handles via [`DNS_QUERIES`] and returns `None` for anything
//! else, so the caller's `GetAddrInfoExCancel` falls through to the original
//! (never reaching real ws2_32 with a synthetic/NULL handle).

use std::{
    borrow::Borrow,
    ptr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use mirrord_layer_lib::{
    detour::Detour,
    error::HookError,
    socket::dns::windows::{
        MANAGED_ADDRINFO, resolve_to_managed,
        utils::{ManagedAddrInfo, ManagedAddrInfoAny},
    },
};
use once_cell::sync::Lazy;
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{DWORD, LPHANDLE},
        ntdef::HANDLE,
        winerror::NO_ERROR,
        ws2def::{ADDRINFOEXW, PADDRINFOEXW},
    },
    um::{
        minwinbase::OVERLAPPED,
        synchapi::SetEvent,
        winsock2::{
            LPWSAOVERLAPPED, WSA_E_CANCELLED, WSA_INVALID_HANDLE, WSA_IO_PENDING,
            WSAHOST_NOT_FOUND, WSATRY_AGAIN,
        },
        ws2tcpip::LPLOOKUPSERVICE_COMPLETION_ROUTINE,
    },
};

use crate::managed::{
    ManagedRegistry,
    handle::{CounterAllocated, ManagedHandleKey},
};

/// The completion routine ABI: `void CALLBACK(DWORD error, DWORD bytes, LPWSAOVERLAPPED ov)`.
type CompletionRoutineFn = unsafe extern "system" fn(DWORD, DWORD, LPWSAOVERLAPPED);

// `OVERLAPPED.Internal` is read by `GetAddrInfoExOverlappedResult` as an
// NTSTATUS (not a Winsock error). There is no public Winsock->NTSTATUS
// converter, so we report only success vs. generic failure here; the precise
// error code is delivered to the caller via the completion routine's `dwError`
// argument (which is what .NET and the MSDN sample actually consume).
const STATUS_SUCCESS: usize = 0x0000_0000;
const STATUS_UNSUCCESSFUL: usize = 0xC000_0001;

/// One in-flight async `GetAddrInfoExW`.
///
/// All raw pointers are stored as `usize`, so the struct is `Send`/`Sync` without an unsafe impl.
///
/// The caller's context owns the `OVERLAPPED` and the `ppResult` slot. By contract it outlives the
/// async operation, so those pointers stay valid until we deliver.
struct AsyncQuery {
    /// Single-fire latch shared by the worker and any cancel.
    completed: AtomicBool,
    /// `*mut OVERLAPPED` (may be 0 if only a completion routine was given).
    overlapped: usize,
    /// [`CompletionRoutineFn`] as `usize` (0 if the caller passed none).
    routine: usize,
    /// `*mut PADDRINFOEXW` — the caller's out-param for the result chain head.
    ppresult: usize,
}

impl AsyncQuery {
    /// Win the right to complete this query. At most one caller gets `true`.
    fn claim(&self) -> bool {
        self.completed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Deliver the result and fire the caller's completion mechanism.
    ///
    /// # Safety
    /// Must be called at most once per query (gate with [`claim`](AsyncQuery::claim)). The stored
    /// `overlapped`/`ppresult` pointers must still be valid (they are, by the
    /// async contract: the caller keeps its context alive until completion).
    unsafe fn deliver(&self, error: DWORD, head: PADDRINFOEXW) {
        // Write the caller's out-param (`*ppResult`).
        if self.ppresult != 0 {
            unsafe { *(self.ppresult as *mut PADDRINFOEXW) = head };
        }

        let ov = self.overlapped as *mut OVERLAPPED;
        if !ov.is_null() {
            // MSDN (GetAddrInfoExW, completion-routine remarks):
            //   "The lpOverlapped parameter ... The Pointer member of the
            //    OVERLAPPED structure will be set to the value of the ppResult
            //    parameter of the original call."
            // i.e. `Pointer` holds the result list head (`*ppResult`) so a
            // caller can recover the results from the overlapped alone — NOT the
            // address of the ppResult out-slot. This is what callers consume;
            // .NET ignores `Pointer` entirely and reads `context->Result`
            // (== `*ppResult`), so writing `head` here is correct either way.
            // <https://learn.microsoft.com/en-us/windows/win32/api/ws2tcpip/nf-ws2tcpip-getaddrinfoexw>
            //
            // `Internal` is an NTSTATUS read by GetAddrInfoExOverlappedResult
            // (success vs. generic failure here; precise error -> dwError).
            unsafe {
                *(*ov).u.Pointer_mut() = head as *mut c_void;
                (*ov).Internal = if error == NO_ERROR {
                    STATUS_SUCCESS
                } else {
                    STATUS_UNSUCCESSFUL
                };
            }
        }

        if self.routine != 0 {
            // SAFETY: `routine` was produced from a valid CompletionRoutineFn
            // pointer in `begin`; transmuting the usize back is the inverse.
            let routine: CompletionRoutineFn = unsafe { std::mem::transmute(self.routine) };
            unsafe { routine(error, 0, ov as LPWSAOVERLAPPED) };
        } else if !ov.is_null() {
            let event = unsafe { (*ov).hEvent };
            if !event.is_null() {
                unsafe { SetEvent(event) };
            }
        }
    }
}

/// Synthetic cancel handle the layer hands out for an in-flight async query.
///
/// Minted from the managed-handle registry's counter in a `0x6000_xxxx` range. That range is
/// disjoint from OS handles and from file handles (`0x5000_xxxx`).
///
/// So any `GetAddrInfoExCancel` on one is unambiguously ours, and the value never collides with
/// NULL.
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct DnsQueryHandle(HANDLE);

// SAFETY: the wrapped HANDLE is an opaque synthetic value we mint ourselves and
// never dereference, so sharing it across threads is sound.
unsafe impl Send for DnsQueryHandle {}
unsafe impl Sync for DnsQueryHandle {}

impl Borrow<HANDLE> for DnsQueryHandle {
    fn borrow(&self) -> &HANDLE {
        &self.0
    }
}

impl ManagedHandleKey for DnsQueryHandle {}

impl CounterAllocated for DnsQueryHandle {
    const FIRST: usize = 0x6000_0000;

    fn from_raw(raw: usize) -> Self {
        Self(raw as _)
    }
}

/// Live async queries, keyed by the synthetic cancel handle we hand out. An
/// entry exists from [`begin`] until whichever of {worker, cancel} completes it.
static DNS_QUERIES: Lazy<ManagedRegistry<DnsQueryHandle, AsyncQuery>> =
    Lazy::new(|| ManagedRegistry::new("DNS_QUERIES"));

/// Look up the shared query behind a raw handle value. The registry's
/// `Borrow<HANDLE>` key lets us query by the value the caller passed back, and
/// its sharded lock spins briefly on contention so a transient conflict isn't
/// mistaken for "not one of ours".
fn lookup(handle: HANDLE) -> Option<Arc<RwLock<AsyncQuery>>> {
    DNS_QUERIES.get(&handle)
}

/// Remove a completed query from the registry.
fn forget(handle: HANDLE) {
    DNS_QUERIES.remove(&handle);
}

/// Owns the worker's obligation to complete the query exactly once. On `Drop`
/// (including panic unwind), if nothing was delivered, fires a failure so the
/// caller does not hang.
struct CompletionGuard {
    handle: DnsQueryHandle,
    query: Arc<RwLock<AsyncQuery>>,
    fired: bool,
}

/// Publish a freshly-built ADDRINFOEXW chain into the global map (so the
/// `FreeAddrInfoExW` hook can reclaim it) and return its head.
///
/// ## ⚠️ Load-bearing lock scope
///
/// The `MANAGED_ADDRINFO` mutex is acquired AND released entirely within this
/// function — it must NEVER be held across a subsequent `deliver()` call.
///
/// `deliver()` invokes the caller's completion routine, which for .NET
/// synchronously calls `FreeAddrInfoExW` -> `free_managed_addrinfo`, re-locking
/// `MANAGED_ADDRINFO` on the SAME THREAD. It is a non-reentrant
/// `std::sync::Mutex`, so holding it across `deliver()` self-deadlocks.
///
/// Keeping the lock confined to this helper makes that mistake structurally
/// impossible: callers get back only a raw pointer, never a lock guard.
fn publish_chain(managed: ManagedAddrInfo<ADDRINFOEXW>) -> PADDRINFOEXW {
    let head = managed.as_ptr();
    let mut map = MANAGED_ADDRINFO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.insert(head as usize, ManagedAddrInfoAny::Ex(managed));
    head
}

impl CompletionGuard {
    fn succeed(&mut self, managed: ManagedAddrInfo<ADDRINFOEXW>) {
        if self.fired {
            return;
        }
        self.fired = true;
        // We only ever read-lock the per-entry RwLock (AsyncQuery is interior-
        // atomic), so this never contends a writer and is safe to hold across
        // the re-entrant deliver() below.
        let query = self.query.read().unwrap_or_else(|p| p.into_inner());
        if query.claim() {
            // `publish_chain` confines the MANAGED_ADDRINFO lock; it is released
            // before `deliver()` runs (see the warning on `publish_chain` —
            // `deliver` re-enters our FreeAddrInfoExW hook on this thread).
            // `deliver` performs only pointer writes + the FFI completion call
            // and must not unwind; the worker pool's `catch_unwind` keeps the
            // thread alive even if it somehow did (the map entry would then leak
            // rather than corrupt — an acceptable trade for an impossible case).
            let head = publish_chain(managed);
            // SAFETY: we won the claim, so this is the only deliver for this query.
            unsafe { query.deliver(NO_ERROR, head) };
            drop(query);
            forget(self.handle.0);
        }
        // else: a cancel already delivered; `managed` is dropped here (freed),
        // never having been published to MANAGED_ADDRINFO — no leak.
    }

    fn fail(&mut self, error: DWORD) {
        if self.fired {
            return;
        }
        self.fired = true;
        let query = self.query.read().unwrap_or_else(|p| p.into_inner());
        if query.claim() {
            // SAFETY: we won the claim; null head is valid for an error completion.
            unsafe { query.deliver(error, ptr::null_mut()) };
            drop(query);
            forget(self.handle.0);
        }
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        // Worker panicked or returned without delivering: unblock the caller.
        if !self.fired {
            self.fail(WSAHOST_NOT_FOUND as DWORD);
        }
    }
}

/// Start an asynchronous resolution. Returns `WSA_IO_PENDING`; the caller's
/// completion mechanism fires later from a worker thread (or a cancel).
///
/// `node`/`port`/`ai_*` are captured by value because the caller's `hints`
/// pointer and name buffers are not guaranteed to outlive the call — only its
/// `OVERLAPPED`/`ppResult` context is, and those are passed as raw pointers.
///
/// # Safety
/// `pp_result`, `overlapped`, and `out_handle` must be the pointers the caller
/// passed to `GetAddrInfoExW`, valid until completion.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn begin(
    node: String,
    port: u16,
    ai_family: i32,
    ai_socktype: i32,
    ai_protocol: i32,
    pp_result: *mut PADDRINFOEXW,
    overlapped: *mut OVERLAPPED,
    routine: LPLOOKUPSERVICE_COMPLETION_ROUTINE,
    out_handle: LPHANDLE,
) -> i32 {
    // Mint a synthetic cancel handle and register the query under it. The counter starts at
    // 0x6000_0000, so the handle is disjoint from OS/file handles and never NULL.
    //
    // `insert` spreads inserts across the registry's lock shards. Under pathological same-shard
    // contention it blocks to acquire rather than dropping one. So a burst of concurrent
    // resolutions (e.g. a .NET HttpClient firing many at once) never spuriously fails to register.
    //
    // Registration therefore always succeeds. There is no synchronous-failure path here.
    let query = AsyncQuery {
        completed: AtomicBool::new(false),
        overlapped: overlapped as usize,
        routine: routine.map_or(0, |f| f as usize),
        ppresult: pp_result as usize,
    };
    let handle = DNS_QUERIES.insert(query);

    // Hand the caller our synthetic cancel handle (never a real ws2_32 handle).
    if !out_handle.is_null() {
        unsafe { *out_handle = handle.0 };
    }

    let Some(query_arc) = lookup(handle.0) else {
        // The just-inserted entry couldn't be read back (only under extreme
        // lock contention). Clean up and fail rather than orphan the caller.
        forget(handle.0);
        return WSAHOST_NOT_FOUND;
    };

    crate::task_pool::submit(move || {
        let mut guard = CompletionGuard {
            handle,
            query: query_arc,
            fired: false,
        };
        match resolve_to_managed::<ADDRINFOEXW>(node, port, ai_family, ai_socktype, ai_protocol) {
            Detour::Success(managed) => guard.succeed(managed),
            Detour::Bypass(reason) => {
                // The selector check already ran synchronously before `begin`,
                // so a bypass here is unexpected; treat as "not found".
                tracing::debug!(
                    ?reason,
                    "GetAddrInfoExW async: resolution bypassed, failing"
                );
                guard.fail(WSAHOST_NOT_FOUND as DWORD);
            }
            Detour::Error(err) => {
                // Distinguish a genuine empty result (the name truly has no
                // records -> host-not-found, terminal) from a transient proxy/
                // comms failure (-> try-again, so the caller can retry rather
                // than cache a bogus NXDOMAIN). In the async path .NET surfaces
                // both as a SocketException via the callback's dwError.
                let code = match &err {
                    HookError::DNSNoName => WSAHOST_NOT_FOUND,
                    _ => WSATRY_AGAIN,
                };
                tracing::debug!(%err, code, "GetAddrInfoExW async: resolution error, failing");
                guard.fail(code as DWORD);
            }
        }
        // `guard` drops here; if neither branch delivered (impossible above,
        // but defends against future edits / panics), Drop fires a failure.
    });

    WSA_IO_PENDING
}

/// Handle `GetAddrInfoExCancel(lpHandle)` for a synthetic handle.
///
/// Returns `None` if `handle_value` is not one of ours (caller should fall back
/// to the original `GetAddrInfoExCancel`). Otherwise returns the code to report:
/// `NO_ERROR` if we initiated cancellation, `WSA_INVALID_HANDLE` if the query
/// had already completed.
///
/// Per MSDN (GetAddrInfoExCancel), a successful cancel "immediately invokes the
/// user's completion mechanism" with the error set to `WSA_E_CANCELLED`, which
/// is exactly what winning the claim and delivering does here.
/// <https://learn.microsoft.com/en-us/windows/win32/api/ws2tcpip/nf-ws2tcpip-getaddrinfoexcancel>
pub(crate) unsafe fn cancel(handle_value: usize) -> Option<i32> {
    let query_arc = lookup(handle_value as HANDLE)?;
    let query = query_arc.read().unwrap_or_else(|p| p.into_inner());
    if query.claim() {
        // SAFETY: we won the claim; this is the only deliver for this query.
        unsafe { query.deliver(WSA_E_CANCELLED as DWORD, ptr::null_mut()) };
        drop(query);
        forget(handle_value as HANDLE);
        Some(NO_ERROR as i32)
    } else {
        // The worker won the claim; it will deliver the completion (it may not
        // have fired it yet). Returning WSA_INVALID_HANDLE here is safe and does
        // NOT race the worker's write into the caller's OVERLAPPED/ppResult,
        // because a compliant caller must keep that context alive until the
        // completion routine runs regardless of the cancel result. Per MSDN
        // (GetAddrInfoExW), the cancel handle "is valid when GetAddrInfoEx
        // returns until the completion routine is called, the event is
        // triggered, or GetAddrInfoExCancel is called" — i.e. the caller awaits
        // the callback, which the worker still delivers.
        // <https://learn.microsoft.com/en-us/windows/win32/api/ws2tcpip/nf-ws2tcpip-getaddrinfoexw>
        drop(query);
        forget(handle_value as HANDLE);
        Some(WSA_INVALID_HANDLE)
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests for the async `GetAddrInfoExW` machinery. These run
    //! in-process against the real Win32 APIs (no cluster / proxy): they
    //! exercise the completion delivery, the single-fire/cancel race
    //! resolution, the fire-on-drop guard, and the ADDRINFOEXW chain we build
    //! vs. what the OS produces. Build/run via `cargo test -p mirrord-layer-win`.

    use std::{
        net::{IpAddr, Ipv4Addr},
        ptr,
        sync::atomic::{AtomicU32, Ordering},
    };

    use winapi::{
        shared::ws2def::{AF_INET, AI_NUMERICHOST, NS_DNS, SOCKADDR_IN},
        um::{
            handleapi::CloseHandle,
            minwinbase::OVERLAPPED,
            synchapi::{CreateEventW, WaitForSingleObject},
            winsock2::{LPWSAOVERLAPPED, SOCK_STREAM, WSACleanup, WSADATA, WSAStartup},
            ws2tcpip::{FreeAddrInfoExW, GetAddrInfoExW},
        },
    };

    // Brings AsyncQuery, CompletionGuard, DNS_QUERIES, lookup, cancel,
    // ManagedAddrInfo, ADDRINFOEXW, PADDRINFOEXW, NS_DNS, error consts, etc.
    use super::*;

    /// Mirrors the way a real caller (e.g. .NET) recovers its context from the
    /// `OVERLAPPED*` passed to the completion routine: the `OVERLAPPED` is the
    /// first field, so the routine casts the pointer back to `*mut TestCtx`.
    /// Keeping the context on the test's stack avoids any cross-test global
    /// state, so the tests are safe under cargo's parallel runner.
    #[repr(C)]
    struct TestCtx {
        ov: OVERLAPPED,
        called: AtomicU32,
        last_err: AtomicU32,
    }

    impl TestCtx {
        fn new() -> Self {
            Self {
                // SAFETY: OVERLAPPED is a plain-old-data struct; all-zero is valid.
                ov: unsafe { std::mem::zeroed() },
                called: AtomicU32::new(0),
                last_err: AtomicU32::new(u32::MAX),
            }
        }
    }

    unsafe extern "system" fn record_routine(
        error: DWORD,
        _bytes: DWORD,
        overlapped: LPWSAOVERLAPPED,
    ) {
        // `overlapped` points at `TestCtx.ov`, the first field, so it is also a
        // `*mut TestCtx`.
        let ctx = overlapped as *mut TestCtx;
        unsafe {
            (*ctx).called.fetch_add(1, Ordering::SeqCst);
            (*ctx).last_err.store(error, Ordering::SeqCst);
        }
    }

    fn query_with(ctx: &mut TestCtx, routine: bool, ppresult: *mut PADDRINFOEXW) -> AsyncQuery {
        AsyncQuery {
            completed: AtomicBool::new(false),
            overlapped: (&mut ctx.ov as *mut OVERLAPPED) as usize,
            routine: if routine {
                record_routine as *const () as usize
            } else {
                0
            },
            ppresult: ppresult as usize,
        }
    }

    /// Insert a query into the shared registry. `insert` always succeeds (it blocks under extreme
    /// contention), so there is nothing to retry.
    fn insert_query(ctx: &mut TestCtx, routine: bool, slot: *mut PADDRINFOEXW) -> DnsQueryHandle {
        DNS_QUERIES.insert(query_with(ctx, routine, slot))
    }

    const SENTINEL_HEAD: usize = 0xABCD_0000;

    /// deliver() via a completion routine writes *ppResult, sets OVERLAPPED
    /// Pointer + Internal, and invokes the routine exactly once with the error.
    #[test]
    fn deliver_invokes_completion_routine() {
        let mut ctx = TestCtx::new();
        let mut slot: PADDRINFOEXW = ptr::null_mut();
        let query = query_with(&mut ctx, true, &mut slot);
        let head = SENTINEL_HEAD as PADDRINFOEXW;

        assert!(query.claim(), "first claim must win");
        unsafe { query.deliver(0, head) };

        assert_eq!(ctx.called.load(Ordering::SeqCst), 1, "routine called once");
        assert_eq!(ctx.last_err.load(Ordering::SeqCst), 0, "error propagated");
        assert_eq!(slot, head, "*ppResult written");
        assert_eq!(ctx.ov.Internal, 0, "OVERLAPPED.Internal = error");
        // OVERLAPPED.Pointer (the contract field) round-trips the result head.
        let pointer = unsafe { *ctx.ov.u.Pointer_mut() } as usize;
        assert_eq!(pointer, SENTINEL_HEAD, "OVERLAPPED.Pointer = *ppResult");
    }

    /// With no routine, deliver() signals the OVERLAPPED's event instead.
    #[test]
    fn deliver_signals_event_when_no_routine() {
        let mut ctx = TestCtx::new();
        let event = unsafe { CreateEventW(ptr::null_mut(), 1, 0, ptr::null()) };
        assert!(!event.is_null(), "CreateEventW");
        ctx.ov.hEvent = event;

        let mut slot: PADDRINFOEXW = ptr::null_mut();
        let query = query_with(&mut ctx, false, &mut slot);

        assert!(query.claim());
        unsafe { query.deliver(0, SENTINEL_HEAD as PADDRINFOEXW) };

        // 0 == WAIT_OBJECT_0: the event is signaled.
        let waited = unsafe { WaitForSingleObject(event, 0) };
        unsafe { CloseHandle(event) };
        assert_eq!(waited, 0, "event signaled");
        assert_eq!(ctx.called.load(Ordering::SeqCst), 0, "no routine to call");
    }

    /// The single-fire latch: exactly one of {worker, cancel} can complete.
    #[test]
    fn claim_is_single_fire() {
        let mut ctx = TestCtx::new();
        let mut slot: PADDRINFOEXW = ptr::null_mut();
        let query = query_with(&mut ctx, true, &mut slot);

        assert!(query.claim(), "first claim wins");
        assert!(!query.claim(), "second claim loses");
        assert!(!query.claim(), "third claim loses");
    }

    /// cancel() on a registered query delivers WSA_E_CANCELLED once and reports
    /// NO_ERROR; a second cancel of the same handle is not ours (already taken).
    #[test]
    fn cancel_delivers_once_then_invalidates() {
        let mut ctx = TestCtx::new();
        let mut slot: PADDRINFOEXW = ptr::null_mut();
        let handle = insert_query(&mut ctx, true, &mut slot);
        let hv = handle.0 as usize;

        let first = unsafe { cancel(hv) };
        assert_eq!(first, Some(NO_ERROR as i32), "we initiated cancellation");
        assert_eq!(
            ctx.last_err.load(Ordering::SeqCst),
            WSA_E_CANCELLED as u32,
            "completion delivered with WSA_E_CANCELLED"
        );
        assert_eq!(ctx.called.load(Ordering::SeqCst), 1);

        // The entry was claimed + removed: it's gone from the registry, so a
        // second cancel is not ours and a racing worker would find nothing.
        assert!(lookup(handle.0).is_none(), "entry removed after cancel");
        assert_eq!(unsafe { cancel(hv) }, None, "handle no longer registered");
    }

    /// A worker that drops its CompletionGuard without delivering (panic / early
    /// return) still fires a failure, so the caller's callback-less Task can't
    /// hang. Verified by observing the routine fired with WSAHOST_NOT_FOUND.
    #[test]
    fn guard_fires_failure_on_drop() {
        let mut ctx = TestCtx::new();
        let mut slot: PADDRINFOEXW = ptr::null_mut();
        let handle = insert_query(&mut ctx, true, &mut slot);
        let query = lookup(handle.0).expect("lookup just-inserted entry");

        {
            let _guard = CompletionGuard {
                handle,
                query,
                fired: false,
            };
            // never call succeed/fail -> Drop must fire a failure
        }

        assert_eq!(ctx.called.load(Ordering::SeqCst), 1, "guard fired on drop");
        assert_eq!(
            ctx.last_err.load(Ordering::SeqCst),
            WSAHOST_NOT_FOUND as u32,
            "drop delivers host-not-found"
        );
        assert!(lookup(handle.0).is_none(), "guard removed the entry");
    }

    /// The ADDRINFOEXW chain our converter builds matches what the OS's own
    /// `GetAddrInfoExW` produces for the same numeric address: same family,
    /// same sockaddr bytes, and the EX-only blob/provider fields left empty
    /// (as the OS leaves them for a DNS-namespace lookup).
    #[test]
    fn managed_chain_matches_native_addrinfoexw() {
        let mut wsa: WSADATA = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { WSAStartup(0x0202, &mut wsa) }, 0, "WSAStartup");

        // Native, numeric, single stream entry.
        let name: Vec<u16> = "127.0.0.1".encode_utf16().chain(Some(0)).collect();
        let mut hints: ADDRINFOEXW = unsafe { std::mem::zeroed() };
        hints.ai_flags = AI_NUMERICHOST;
        hints.ai_family = AF_INET;
        hints.ai_socktype = SOCK_STREAM;
        let mut native: PADDRINFOEXW = ptr::null_mut();
        let rc = unsafe {
            GetAddrInfoExW(
                name.as_ptr(),
                ptr::null(),
                NS_DNS,
                ptr::null_mut(),
                &hints,
                &mut native,
                ptr::null_mut(),
                ptr::null_mut(),
                None,
                ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "native GetAddrInfoExW(127.0.0.1)");
        assert!(!native.is_null());

        // Ours, from the same address.
        let managed = ManagedAddrInfo::<ADDRINFOEXW>::try_from(vec![(
            "127.0.0.1".to_string(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        )])
        .expect("build managed ADDRINFOEXW");
        let ours = managed.as_ptr();
        assert!(!ours.is_null());

        unsafe {
            let n = &*native;
            let o = &*ours;
            assert_eq!(o.ai_family, AF_INET, "family");
            assert_eq!(o.ai_family, n.ai_family, "family matches native");
            assert_eq!(
                o.ai_addrlen,
                std::mem::size_of::<SOCKADDR_IN>(),
                "addrlen is sizeof(sockaddr_in)"
            );
            // sockaddr family + address bytes match native.
            let os = &*(o.ai_addr as *const SOCKADDR_IN);
            let ns = &*(n.ai_addr as *const SOCKADDR_IN);
            assert_eq!(os.sin_family, ns.sin_family, "sin_family");
            assert_eq!(
                *os.sin_addr.S_un.S_addr(),
                *ns.sin_addr.S_un.S_addr(),
                "sin_addr (127.0.0.1)"
            );
            // EX-only fields we always leave empty (as native does for DNS).
            assert!(o.ai_blob.is_null(), "ai_blob null");
            assert_eq!(o.ai_bloblen, 0, "ai_bloblen 0");
            assert!(o.ai_provider.is_null(), "ai_provider null");
            // Single record -> chain terminates.
            assert!(o.ai_next.is_null(), "ai_next terminates");

            FreeAddrInfoExW(native);
        }
        unsafe { WSACleanup() };
    }
}
