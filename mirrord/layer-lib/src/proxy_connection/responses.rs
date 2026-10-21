use std::{
    collections::HashMap,
    io,
    net::TcpStream,
    sync::{Arc, Condvar, TryLockError},
};

use mirrord_intproxy_protocol::{
    LocalMessage, ProxyToLayerMessage,
    codec::{CodecError, SyncDecoder},
};

use crate::{
    mutex::Mutex,
    proxy_connection::{ProxyError, Result},
};

pub type Decoder = SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>;

/// Manages response reception on a single [`Decoder`] shared by all threads in the current process.
#[derive(Debug)]
pub struct ResponseManager {
    /// Decoder that receives responses from the proxy.
    receiver: Mutex<Result<Decoder, Arc<CodecError>>>,
    /// Stores the state of threads running [`Self::receive`].
    state: Mutex<HashMap<u64, Slot>>,
}

impl ResponseManager {
    pub fn new(receiver: Decoder) -> Self {
        Self {
            receiver: Mutex::new(Ok(receiver)),
            state: Default::default(),
        }
    }

    pub fn receive(&self, response_id: u64) -> Result<ProxyToLayerMessage> {
        let mut state = self.state.lock()?;
        loop {
            // Check if some other thread already received our response.
            // This also clears our stale `Waiting` slot, if any.
            if let Some(Slot::Ready(payload)) = state.remove(&response_id) {
                return Ok(payload);
            }

            let mut receiver = match self.receiver.try_lock() {
                Ok(receiver) => receiver,
                Err(TryLockError::Poisoned(..)) => return Err(ProxyError::LockPoisoned),
                // Some other thread is reading from the socket, wait until it notifies us.
                // We are notified when our response is ready, when the reading thread leaves,
                // or spuriously, so we always recheck the state.
                Err(TryLockError::WouldBlock) => {
                    let condvar = Arc::new(Condvar::new());
                    state.insert(response_id, Slot::Waiting(condvar.clone()));
                    #[cfg(not(test))]
                    {
                        // Many things can go wrong in multi-threaded code injected via dynamic library.
                        // We use a timeout just to be more safe.
                        state = condvar
                            .wait_timeout(state, std::time::Duration::from_millis(100))?
                            .0;
                    }
                    #[cfg(test)]
                    {
                        // In unit tests, we assert that wakeups arrive as expected.
                        state = condvar.wait(state)?;
                    }
                    continue;
                }
            };
            let decoder = receiver.as_mut().map_err(|error| error.clone())?;

            let result = loop {
                drop(state);
                let result = decoder
                    .receive()
                    .and_then(|res| {
                        res.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof).into())
                    })
                    .map_err(Arc::new);
                state = self.state.lock()?;
                match result {
                    Ok(response) if response.message_id == response_id => break Ok(response.inner),
                    Ok(response) => {
                        // Store responses meant for other threads, and wake them if they're waiting.
                        if let Some(Slot::Waiting(condvar)) =
                            state.insert(response.message_id, Slot::Ready(response.inner))
                        {
                            condvar.notify_one();
                        }
                    }
                    Err(error) => break Err(error),
                }
            };

            let mut waiting = state.values().filter_map(Slot::waiting);
            match &result {
                // Hand the reading over to one of the waiting threads.
                Ok(..) => {
                    drop(receiver);
                    if let Some(condvar) = waiting.next() {
                        condvar.notify_one();
                    }
                }
                // The stream is no longer usable.
                // Wake all waiting threads.
                Err(error) => {
                    *receiver = Err(error.clone());
                    drop(receiver);
                    waiting.for_each(Condvar::notify_one);
                }
            }

            break result.map_err(From::from);
        }
    }
}

#[derive(Debug)]
enum Slot {
    /// A thread is waiting on this [`Condvar`], paired with [`ResponseManager::state`].
    Waiting(Arc<Condvar>),
    Ready(ProxyToLayerMessage),
}

impl Slot {
    fn waiting(&self) -> Option<&Condvar> {
        match self {
            Self::Waiting(cond) => Some(cond),
            Self::Ready(..) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        net::{TcpListener, TcpStream},
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use mirrord_intproxy_protocol::{
        LayerId, LocalMessage, ProxyToLayerMessage,
        codec::{CodecError, SyncDecoder, SyncEncoder},
    };

    use super::{ResponseManager, Slot};
    use crate::proxy_connection::{ProxyError, Result};

    /// Bounds every wait in these tests, so that a broken [`ResponseManager`] fails them instead
    /// of hanging them.
    const TIMEOUT: Duration = Duration::from_secs(5);

    type Proxy = SyncEncoder<LocalMessage<ProxyToLayerMessage>, TcpStream>;

    /// Returns a [`ResponseManager`] and the proxy side of its connection.
    fn setup() -> (Arc<ResponseManager>, Proxy) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let layer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (proxy, _) = listener.accept().unwrap();

        let manager = ResponseManager::new(SyncDecoder::new(layer));
        (Arc::new(manager), SyncEncoder::new(proxy))
    }

    /// Payload unique to the given response id.
    fn payload(message_id: u64) -> ProxyToLayerMessage {
        ProxyToLayerMessage::NewSession(LayerId(message_id))
    }

    fn respond(proxy: &mut Proxy, message_id: u64) {
        proxy
            .send(&LocalMessage {
                message_id,
                inner: payload(message_id),
            })
            .unwrap();
        proxy.flush().unwrap();
    }

    fn spawn_receive(
        manager: &Arc<ResponseManager>,
        response_id: u64,
    ) -> mpsc::Receiver<Result<ProxyToLayerMessage>> {
        let manager = manager.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // The test may have already failed and dropped `rx`.
            let _ = tx.send(manager.receive(response_id));
        });
        rx
    }

    fn wait_until(condition: impl Fn() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < TIMEOUT,
                "condition not met in {TIMEOUT:?}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn is_reading(manager: &ResponseManager) -> bool {
        // The threads under test only try the receiver while holding `state`. Holding it here
        // keeps them from finding the receiver taken by this check and waiting for a notification
        // that never comes.
        let _state = manager.state.lock().unwrap();
        manager.receiver.try_lock().is_err()
    }

    fn is_waiting(manager: &ResponseManager, response_id: u64) -> bool {
        matches!(
            manager.state.lock().unwrap().get(&response_id),
            Some(Slot::Waiting(..))
        )
    }

    fn assert_connection_closed(result: Result<ProxyToLayerMessage>) -> Arc<CodecError> {
        match result {
            Err(ProxyError::CodecError(error)) if matches!(&*error, CodecError::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof) => {
                error
            }
            other => panic!("expected connection closed error, got {other:?}"),
        }
    }

    /// Responses read while looking for another one are kept for later calls.
    #[test]
    fn out_of_order_responses() {
        let (manager, mut proxy) = setup();
        for message_id in [3, 2, 1] {
            respond(&mut proxy, message_id);
        }

        for response_id in [1, 2, 3] {
            assert_eq!(manager.receive(response_id).unwrap(), payload(response_id));
        }
    }

    /// A thread gets its response as soon as it arrives, even though another thread is still
    /// reading the connection, waiting for its own response.
    #[test]
    fn response_delivered_to_waiting_thread() {
        let (manager, mut proxy) = setup();

        let reader = spawn_receive(&manager, 1);
        wait_until(|| is_reading(&manager));
        let waiter = spawn_receive(&manager, 2);
        wait_until(|| is_waiting(&manager, 2));

        respond(&mut proxy, 2);
        assert_eq!(waiter.recv_timeout(TIMEOUT).unwrap().unwrap(), payload(2));
        assert!(reader.try_recv().is_err());

        respond(&mut proxy, 1);
        assert_eq!(reader.recv_timeout(TIMEOUT).unwrap().unwrap(), payload(1));
    }

    /// When the reading thread gets its response, a waiting thread takes over the reading.
    #[test]
    fn reading_handed_over() {
        let (manager, mut proxy) = setup();

        let reader = spawn_receive(&manager, 1);
        wait_until(|| is_reading(&manager));
        let waiter = spawn_receive(&manager, 2);
        wait_until(|| is_waiting(&manager, 2));

        respond(&mut proxy, 1);
        assert_eq!(reader.recv_timeout(TIMEOUT).unwrap().unwrap(), payload(1));

        respond(&mut proxy, 2);
        assert_eq!(waiter.recv_timeout(TIMEOUT).unwrap().unwrap(), payload(2));
    }

    /// A connection failure is reported to every thread, including ones that call
    /// [`ResponseManager::receive`] later, and they all get the original error.
    #[test]
    fn error_reaches_all_threads() {
        let (manager, proxy) = setup();

        let reader = spawn_receive(&manager, 1);
        wait_until(|| is_reading(&manager));
        let waiter = spawn_receive(&manager, 2);
        wait_until(|| is_waiting(&manager, 2));

        drop(proxy);
        let reader_error = assert_connection_closed(reader.recv_timeout(TIMEOUT).unwrap());
        let waiter_error = assert_connection_closed(waiter.recv_timeout(TIMEOUT).unwrap());
        let late_error = assert_connection_closed(manager.receive(3));

        assert!(Arc::ptr_eq(&reader_error, &waiter_error));
        assert!(Arc::ptr_eq(&reader_error, &late_error));
    }

    /// Many threads receiving at once each get their own response.
    #[test]
    fn concurrent_receivers() {
        let (manager, mut proxy) = setup();

        let receivers = (1..=32)
            .map(|response_id| (response_id, spawn_receive(&manager, response_id)))
            .collect::<Vec<_>>();
        for message_id in (1..=32).rev() {
            respond(&mut proxy, message_id);
        }

        for (response_id, receiver) in receivers {
            assert_eq!(
                receiver.recv_timeout(TIMEOUT).unwrap().unwrap(),
                payload(response_id)
            );
        }
    }
}
