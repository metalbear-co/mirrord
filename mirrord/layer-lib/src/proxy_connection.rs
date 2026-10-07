//! Common ProxyConnection implementation shared between Unix and Windows layers.
use std::{
    collections::HashMap,
    fmt::Debug,
    io,
    net::{SocketAddr, TcpStream},
    sync::{
        OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use mirrord_intproxy_protocol::{
    IsLayerRequest, IsLayerRequestWithResponse, LayerId, LayerToProxyMessage, LocalMessage,
    MessageId, NewSessionRequest, ProxyToLayerMessage,
    codec::{self, CodecError, SyncDecoder, SyncEncoder},
};
use thiserror::Error;

use crate::{
    error::{HookError, HookResult},
    mutex::Mutex,
};

// TODO: We don't really need a lock, we just need a type that:
//  1. Can be initialized as static (with a const constructor or whatever)
//  2. Is `Sync` (because shared static vars have to be).
//  3. Can replace the held [`ProxyConnection`] with a different one (because we need to reset it on
//     `fork`).
//  We only ever set it in the ctor or in the `fork` hook (in the child process), and in both cases
//  there are no other threads yet in that process, so we don't need write synchronization.
//  Assuming it's safe to call `send` simultaneously from two threads, on two references to the
//  same `Sender` (is it), we also don't need read synchronization.
/// Global connection to the internal proxy.
/// Should not be used directly. Use [`make_proxy_request_with_response`] or
/// [`make_proxy_request_no_response`] functions instead.
pub static mut PROXY_CONNECTION: OnceLock<ProxyConnection> = OnceLock::new();

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("{0}")]
    CodecError(#[from] CodecError),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("unexpected response: {0:?}")]
    UnexpectedResponse(
        /// Boxed due to large size difference.
        Box<ProxyToLayerMessage>,
    ),
    #[error("critical error: {0}")]
    ProxyFailure(String),
    #[error("{0}")]
    AgentReportedError(String),
    #[error("connection lock poisoned")]
    LockPoisoned,
    #[error("{0}")]
    IoFailed(#[from] io::Error),
}

impl<T> From<PoisonError<T>> for ProxyError {
    fn from(_value: PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}

pub type Result<T> = core::result::Result<T, ProxyError>;

#[derive(Debug)]
pub struct ProxyConnection {
    sender: Mutex<SyncEncoder<LocalMessage<LayerToProxyMessage>, TcpStream>>,
    responses: Mutex<ResponseManager>,
    next_message_id: AtomicU64,
    layer_id: LayerId,
    proxy_addr: SocketAddr,
}

impl ProxyConnection {
    pub fn new(
        proxy_addr: SocketAddr,
        session: NewSessionRequest,
        timeout: Duration,
    ) -> Result<Self> {
        let connection = TcpStream::connect(proxy_addr)?;
        connection.set_read_timeout(Some(timeout))?;
        connection.set_write_timeout(Some(timeout))?;
        // Layer requests are small and strictly request-response, so Nagle's algorithm only
        // adds latency to every hooked libc call. Failing to set it costs latency, not
        // correctness, so it must not fail the connection.
        if let Err(error) = connection.set_nodelay(true) {
            tracing::warn!(%error, "Failed to set TCP_NODELAY on the internal proxy connection");
        }

        let (mut sender, receiver) = codec::make_sync_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(connection)?;

        sender.send(&LocalMessage {
            message_id: 0,
            inner: LayerToProxyMessage::NewSession(session),
        })?;

        let mut responses = ResponseManager::new(receiver);
        let response = responses.receive(0)?;
        let ProxyToLayerMessage::NewSession(layer_id) = &response else {
            return Err(ProxyError::UnexpectedResponse(Box::new(response)));
        };

        Ok(Self {
            sender: Mutex::new(sender),
            responses: Mutex::new(responses),
            next_message_id: AtomicU64::new(1),
            layer_id: *layer_id,
            proxy_addr,
        })
    }

    fn next_message_id(&self) -> MessageId {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn send(&self, message: LayerToProxyMessage) -> Result<MessageId> {
        let message_id = self.next_message_id();
        let message = LocalMessage {
            message_id,
            inner: message,
        };

        let mut guard = self.sender.lock()?;
        guard.send(&message)?;
        guard.flush()?;

        Ok(message_id)
    }

    pub fn receive(&self, response_id: u64) -> Result<ProxyToLayerMessage> {
        let response = self.responses.lock()?.receive(response_id)?;
        match response {
            ProxyToLayerMessage::ProxyFailed {
                agent_reported: true,
                message: error,
            } => Err(ProxyError::AgentReportedError(error)),
            ProxyToLayerMessage::ProxyFailed {
                agent_reported: false,
                message: error,
            } => Err(ProxyError::ProxyFailure(error)),
            _ => Ok(response),
        }
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_with_response<T>(&self, request: T) -> Result<T::Response>
    where
        T: IsLayerRequestWithResponse + Debug,
        T::Response: Debug,
    {
        let response_id = self.send(request.wrap())?;
        let response = self.receive(response_id)?;
        T::try_unwrap_response(response)
            .map_err(Box::new)
            .map_err(ProxyError::UnexpectedResponse)
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_no_response<T: IsLayerRequest + Debug>(
        &self,
        request: T,
    ) -> Result<MessageId> {
        self.send(request.wrap())
    }

    pub fn layer_id(&self) -> LayerId {
        self.layer_id
    }

    pub fn proxy_addr(&self) -> SocketAddr {
        self.proxy_addr
    }
}

#[derive(Debug)]
struct ResponseManager {
    receiver: SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>,
    outstanding_responses: HashMap<u64, ProxyToLayerMessage>,
}

impl ResponseManager {
    fn new(receiver: SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>) -> Self {
        Self {
            receiver,
            outstanding_responses: Default::default(),
        }
    }

    fn receive(&mut self, response_id: u64) -> Result<ProxyToLayerMessage> {
        if let Some(response) = self.outstanding_responses.remove(&response_id) {
            return Ok(response);
        }

        loop {
            let response = self
                .receiver
                .receive()?
                .ok_or(ProxyError::ConnectionClosed)?;

            if response.message_id == response_id {
                break Ok(response.inner);
            }

            self.outstanding_responses
                .insert(response.message_id, response.inner);
        }
    }
}

/// Time a hooked call waits for the proxy connection to be established before falling
/// back to the pre-connection error path. Matches the worker's own connect timeout.
#[cfg(target_os = "windows")]
const PROXY_CONNECTION_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Publishes the layer's proxy connection and releases every hooked call waiting for it.
///
/// The Windows layer's startup worker calls this once the connection and its handshake are done.
///
/// # Errors
///
/// When a connection is already installed.
#[cfg(target_os = "windows")]
#[allow(static_mut_refs)]
pub fn install_proxy_connection(connection: ProxyConnection) -> crate::error::LayerResult<()> {
    // SAFETY: only the startup worker sets the connection; readers go through `OnceLock::get`.
    unsafe { PROXY_CONNECTION.set(connection) }
        .map_err(|_| crate::error::LayerError::GlobalAlreadyInitialized("PROXY_CONNECTION"))?;
    connection_gate::GATE.publish(connection_gate::State::Ready);
    Ok(())
}

/// Tells every hooked call waiting in [`proxy_connection`] that the connection will never come.
///
/// The layer's startup calls this as soon as it fails, before anything that can block, so no
/// waiter spends the rest of [`PROXY_CONNECTION_WAIT_TIMEOUT`] on it. A layer in trace-only mode,
/// which never connects, calls it before any hook is enabled, so none of its calls waits.
#[cfg(target_os = "windows")]
pub fn abandon_proxy_connection() {
    connection_gate::GATE.publish(connection_gate::State::Abandoned);
}

/// Resolve the global proxy connection, waiting for the layer worker to establish it.
///
/// The Windows layer installs hooks before the proxy connection exists (so app code that
/// starts early - APC/IAT injection - is still intercepted), which means a hooked call can
/// arrive while [`PROXY_CONNECTION`] is unset. Waiting keeps that early call remote instead
/// of silently falling back to a local operation. Trace-only mode never connects and says so at
/// startup (see `abandon_proxy_connection`), and non-Windows layers establish the connection
/// before any hook can fire, so both keep the instant error. So does a call that cannot wait (see
/// `may_wait_for_proxy_connection`, Windows only).
///
/// # Errors
///
/// [`HookError::CannotGetProxyConnection`] when there is no connection after the wait. What that
/// means for the call is up to its hook: some answer the application with an error, not with a
/// local operation.
#[allow(static_mut_refs)]
fn proxy_connection() -> HookResult<&'static ProxyConnection> {
    #[cfg(target_os = "windows")]
    if unsafe { PROXY_CONNECTION.get() }.is_none() {
        connection_gate::GATE.wait(PROXY_CONNECTION_WAIT_TIMEOUT);
    }

    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or(HookError::CannotGetProxyConnection)
    }
}

/// Where the Windows layer's proxy connection stands, for hooked calls that arrive before it
/// exists.
///
/// The state answers "is it there, or will it never be" without a wait; a manual-reset event wakes
/// the calls that do wait. The state is published before the event is set, so a woken waiter
/// always sees it. Both are plain atomics and raw Win32 calls, which are safe under the loader
/// lock and touch no Rust thread state (see the warning in `crate::logging`).
///
/// Either side may be the one that creates the event, and either side may fail to. The publisher
/// stores the state and then looks for the event; a waiter stores the event and then looks at the
/// state again. Every access is sequentially consistent, so at least one of the two sees the
/// other's store: the publisher sets the waiter's event, or the waiter finds the answer and never
/// waits.
#[cfg(target_os = "windows")]
mod connection_gate {
    #[cfg(test)]
    use std::cell::{Cell, RefCell};
    use std::{
        ffi::c_void,
        ptr,
        sync::atomic::{AtomicPtr, AtomicU8, Ordering},
        time::{Duration, Instant},
    };

    use winapi::{
        shared::{minwindef::FALSE, winerror::WAIT_TIMEOUT},
        um::{
            handleapi::CloseHandle,
            synchapi::{CreateEventW, SetEvent, Sleep, WaitForSingleObject},
            winbase::WAIT_OBJECT_0,
        },
    };

    use super::may_wait_for_proxy_connection;

    /// The one gate the layer's hooks wait on.
    pub(super) static GATE: ConnectionGate = ConnectionGate::new();

    /// How often a waiter re-reads the state when it has no event to wait on.
    const POLL_INTERVAL_MS: u32 = 5;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub(super) enum State {
        /// The startup worker is still working on it.
        Pending = 0,
        /// The connection is installed.
        Ready = 1,
        /// The startup gave up; the connection will never exist.
        Abandoned = 2,
    }

    pub(super) struct ConnectionGate {
        state: AtomicU8,
        /// The manual-reset event set once `state` leaves [`State::Pending`]. Created on first
        /// use by whichever side gets there first.
        event: AtomicPtr<c_void>,
    }

    impl ConnectionGate {
        pub(super) const fn new() -> Self {
            Self {
                state: AtomicU8::new(State::Pending as u8),
                event: AtomicPtr::new(ptr::null_mut()),
            }
        }

        fn state(&self) -> State {
            match self.state.load(Ordering::SeqCst) {
                1 => State::Ready,
                2 => State::Abandoned,
                _ => State::Pending,
            }
        }

        /// Moves the gate out of [`State::Pending`], once, and wakes every waiter.
        ///
        /// A gate that already left it keeps its first answer.
        pub(super) fn publish(&self, state: State) {
            if self
                .state
                .compare_exchange(
                    State::Pending as u8,
                    state as u8,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                return;
            }
            // Without an event, waiters poll the state published above.
            if let Some(event) = self.event() {
                unsafe { SetEvent(event.cast()) };
            }
        }

        /// Waits up to `timeout` for the gate to leave [`State::Pending`], when this call can
        /// usefully wait at all.
        ///
        /// The abandoned answer is read first, so a layer whose startup failed answers every
        /// later call without any other work.
        pub(super) fn wait(&self, timeout: Duration) {
            if self.state() != State::Pending || !may_wait_for_proxy_connection() {
                return;
            }

            let deadline = Instant::now() + timeout;
            if let Some(event) = self.event() {
                // The answer may have been published while the event did not exist yet, and a
                // publisher that found no event set none.
                if self.state() != State::Pending {
                    return;
                }
                let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
                match unsafe { WaitForSingleObject(event.cast(), timeout_ms) } {
                    WAIT_OBJECT_0 | WAIT_TIMEOUT => return,
                    result => tracing::debug!(
                        result,
                        error = %std::io::Error::last_os_error(),
                        "waiting on the proxy connection event failed, polling instead"
                    ),
                }
            }
            while self.state() == State::Pending && Instant::now() < deadline {
                unsafe { Sleep(POLL_INTERVAL_MS) };
            }
        }

        /// The gate's event, created on first use.
        ///
        /// # Returns
        ///
        /// `None` when no event could be created; callers then poll [`Self::state`].
        fn event(&self) -> Option<*mut c_void> {
            let current = self.event.load(Ordering::SeqCst);
            if !current.is_null() {
                return Some(current);
            }

            let created = create_event();
            if created.is_null() {
                return None;
            }
            match self.event.compare_exchange(
                ptr::null_mut(),
                created,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => Some(created),
                Err(winner) => {
                    unsafe { CloseHandle(created.cast()) };
                    Some(winner)
                }
            }
        }
    }

    /// Creates an unnamed, nonsignaled, manual-reset event.
    ///
    /// # Returns
    ///
    /// The event, or null when it could not be created.
    fn create_event() -> *mut c_void {
        #[cfg(test)]
        {
            if let Some(before) = BEFORE_EVENT_CREATION.take() {
                before();
            }
            if EVENT_CREATION_FAILS.get() {
                return ptr::null_mut();
            }
        }

        unsafe { CreateEventW(ptr::null_mut(), 1, FALSE, ptr::null()) }.cast()
    }

    #[cfg(test)]
    thread_local! {
        /// Makes every event creation on this thread fail.
        static EVENT_CREATION_FAILS: Cell<bool> = const { Cell::new(false) };
        /// Runs once on this thread, right before its next event creation.
        static BEFORE_EVENT_CREATION: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    impl Drop for ConnectionGate {
        fn drop(&mut self) {
            let event = *self.event.get_mut();
            if !event.is_null() {
                unsafe { CloseHandle(event.cast()) };
            }
        }
    }

    #[cfg(all(test, target_arch = "x86_64"))]
    mod tests {
        use std::{sync::Arc, time::Instant};

        use super::*;
        use crate::proxy_connection::tests::holding_the_loader_lock;

        /// A hooked call from inside a `DllMain` must not wait for a worker that cannot start
        /// until that call returns. The gate is this test's own, still pending, so nothing else
        /// can be what answers.
        #[test]
        fn a_loader_lock_owner_does_not_wait_for_the_proxy() {
            let gate = ConnectionGate::new();
            assert!(may_wait_for_proxy_connection());

            let started = Instant::now();
            let may_wait = holding_the_loader_lock(|| {
                gate.wait(Duration::from_secs(20));
                may_wait_for_proxy_connection()
            });

            assert!(!may_wait, "an owner of the loader lock must not wait");
            assert_eq!(gate.state(), State::Pending);
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "answered at once, took {:?}",
                started.elapsed()
            );
        }

        /// Both ways out of a pending gate release a waiter at once, not at its timeout.
        #[test]
        fn a_published_answer_ends_the_wait() {
            for answer in [State::Abandoned, State::Ready] {
                let gate = Arc::new(ConnectionGate::new());
                let waiter = {
                    let gate = Arc::clone(&gate);
                    std::thread::spawn(move || {
                        let started = Instant::now();
                        gate.wait(Duration::from_secs(20));
                        started.elapsed()
                    })
                };

                std::thread::sleep(Duration::from_millis(100));
                gate.publish(answer);
                let waited = waiter.join().expect("waiter");

                assert_eq!(gate.state(), answer);
                assert!(
                    waited < Duration::from_secs(5),
                    "{answer:?} released the waiter, which waited {waited:?}"
                );
            }
        }

        /// An answer published while no event existed, by a publisher that could not create one
        /// either, still ends the wait of a waiter that creates the event afterwards.
        #[test]
        fn an_answer_published_without_an_event_ends_the_wait() {
            let gate = Arc::new(ConnectionGate::new());
            let publisher = Arc::clone(&gate);
            // Between this waiter's look at the state and its own creation of the event.
            BEFORE_EVENT_CREATION.set(Some(Box::new(move || {
                std::thread::spawn(move || {
                    EVENT_CREATION_FAILS.set(true);
                    publisher.publish(State::Ready);
                })
                .join()
                .expect("publisher");
            })));

            let started = Instant::now();
            gate.wait(Duration::from_secs(20));

            assert_eq!(gate.state(), State::Ready);
            assert!(
                !gate.event.load(Ordering::SeqCst).is_null(),
                "the waiter created the event"
            );
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the published answer ended the wait, which took {:?}",
                started.elapsed()
            );
        }

        /// The first answer stands: a startup that gave up is not revived, and the other way
        /// round.
        #[test]
        fn the_first_answer_stands() {
            let gate = ConnectionGate::new();
            gate.publish(State::Abandoned);
            gate.publish(State::Ready);
            assert_eq!(gate.state(), State::Abandoned);

            let started = Instant::now();
            gate.wait(Duration::from_secs(20));
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}

/// Whether a hooked call on this thread can usefully wait for the proxy connection.
///
/// Only the layer's startup worker establishes the connection, and a new thread cannot start
/// running while another thread holds the loader lock: its `DLL_THREAD_ATTACH` callbacks wait for
/// it. A hooked call made from inside some DLL's `DllMain` (a static import's initializer, or a
/// `LoadLibrary` during startup) would therefore wait out the whole timeout for a worker that
/// cannot run until that call returns. It answers at once instead.
///
/// When ownership cannot be established the answer is also "do not wait": a wasted wait costs
/// the process its whole startup timeout, a skipped one costs one call.
#[cfg(target_os = "windows")]
fn may_wait_for_proxy_connection() -> bool {
    match loader_lock::held_by_current_thread() {
        Some(false) => true,
        held => {
            tracing::debug!(
                loader_lock_held = ?held,
                "not waiting for the proxy connection: this thread may be blocking the worker that establishes it"
            );
            false
        }
    }
}

/// Reads whether the calling thread owns the process loader lock.
///
/// The loader lock is the critical section the PEB points to. Its layout is not a public Windows
/// contract, so this is limited to native x64, where `phnt` describes it, and says "unknown"
/// everywhere else.
#[cfg(target_os = "windows")]
mod loader_lock {
    #[cfg(target_arch = "x86_64")]
    use std::ffi::c_void;

    // Not present in the winapi crate. `phnt` declares it but does not link `ntdll`, so it is
    // bound here with `raw-dylib`.
    #[cfg(target_arch = "x86_64")]
    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn RtlIsCriticalSectionLockedByThread(critical_section: *mut c_void) -> u32;
    }

    /// The loader lock of this process, when it can be found.
    #[cfg(target_arch = "x86_64")]
    pub(super) fn critical_section() -> Option<*mut c_void> {
        // SAFETY: the TEB and the PEB it points to live as long as the thread and the process;
        // both pointers are read, never written.
        unsafe {
            let teb = phnt::ext::NtCurrentTeb();
            let peb = teb.as_ref()?.ProcessEnvironmentBlock;
            let lock = peb.as_ref()?.LoaderLock;
            (!lock.is_null()).then_some(lock.cast())
        }
    }

    /// # Returns
    ///
    /// `Some(true)` when this thread holds the loader lock, `Some(false)` when it does not, and
    /// `None` when that cannot be established.
    #[cfg(target_arch = "x86_64")]
    pub(super) fn held_by_current_thread() -> Option<bool> {
        let lock = critical_section()?;
        // SAFETY: `lock` is the loader lock the PEB names, a critical section that exists for the
        // life of the process. The call only reads its owner.
        Some(unsafe { RtlIsCriticalSectionLockedByThread(lock) } != 0)
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub(super) fn held_by_current_thread() -> Option<bool> {
        None
    }
}

/// Makes a request to the internal proxy using global [`PROXY_CONNECTION`].
/// Blocks until the proxy responds.
pub fn make_proxy_request_with_response<T>(request: T) -> HookResult<T::Response>
where
    T: IsLayerRequestWithResponse + Debug,
    T::Response: Debug,
{
    proxy_connection()?
        .make_request_with_response(request)
        .map_err(Into::into)
}

/// Makes a request to the internal proxy using global [`PROXY_CONNECTION`].
/// Blocks until the request is sent.
pub fn make_proxy_request_no_response<T: IsLayerRequest + Debug>(
    request: T,
) -> HookResult<MessageId> {
    proxy_connection()?
        .make_request_no_response(request)
        .map_err(Into::into)
}

#[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
mod tests {
    use std::ffi::c_void;

    use super::*;

    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn RtlEnterCriticalSection(critical_section: *mut c_void) -> i32;
        fn RtlLeaveCriticalSection(critical_section: *mut c_void) -> i32;
    }

    /// Runs `body` while this thread holds the loader lock, as code inside a `DllMain` does.
    ///
    /// Other threads that start or end in the meantime wait for it, so the body must be short.
    pub(super) fn holding_the_loader_lock<T>(body: impl FnOnce() -> T) -> T {
        let lock = loader_lock::critical_section().expect("the PEB names a loader lock");
        unsafe { RtlEnterCriticalSection(lock) };
        let result = body();
        unsafe { RtlLeaveCriticalSection(lock) };
        result
    }

    #[test]
    fn loader_lock_ownership_is_read_for_this_thread() {
        assert_eq!(loader_lock::held_by_current_thread(), Some(false));
        assert_eq!(
            holding_the_loader_lock(loader_lock::held_by_current_thread),
            Some(true)
        );
        assert_eq!(loader_lock::held_by_current_thread(), Some(false));
    }
}
