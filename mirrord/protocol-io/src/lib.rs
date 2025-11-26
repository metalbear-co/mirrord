//! This module implements mirrord-protocol's wire-level IO.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    io::{self},
    marker::PhantomData,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use bincode::error::DecodeError;
use bytes::{BufMut, BytesMut};
use futures::{Sink, SinkExt, Stream, StreamExt, future::Either};
use mirrord_protocol::{ClientMessage, DaemonMessage, ProtocolCodec};
use rand::seq::IteratorRandom;
use tokio::{
    pin, select,
    sync::{Notify, futures::OwnedNotified, mpsc},
};
use tokio_util::sync::CancellationToken;
use tracing::{Level, instrument};

pub trait AsyncIO: AsyncWrite + AsyncRead + Send + 'static {}
impl<T: AsyncWrite + AsyncRead + Send + 'static> AsyncIO for T {}

pub trait Transport<I, O>
where
    Self: Sink<O> + Stream<Item = Result<I, <Self as Sink<O>>::Error>> + Send + 'static,
{
}

impl<T, I, O> Transport<I, O> for T where
    T: Sink<O> + Stream<Item = Result<I, <Self as Sink<O>>::Error>> + Send + 'static
{
}

/// "Phony" trait, used for determining input and output message types.
///
/// Implemented by [`Client`] and [`Agent`].
pub trait ProtocolEndpoint: 'static + Sized + Clone {
    type InMsg: bincode::Decode<()> + Send + fmt::Debug;
    type OutMsg: bincode::Encode + Send + fmt::Debug;
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

/// Marker type, representing the client side of a mirrord-protocol connection.
#[derive(Clone)]
pub struct Client;

/// Marker type, representing the server side of a mirrord-protocol connection (agent or operator).
#[derive(Clone)]
pub struct Agent;

impl ProtocolEndpoint for Client {
    type InMsg = DaemonMessage;
    type OutMsg = ClientMessage;
}

impl ProtocolEndpoint for Agent {
    type InMsg = ClientMessage;
    type OutMsg = DaemonMessage;
}

// Same as protocolCodec but outputs raw Vec<u8>s
struct Codec<I>(PhantomData<I>);

impl<I: bincode::Decode<()>> Decoder for Codec<I> {
    type Item = I;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        ProtocolCodec::<I, ()>::default().decode(src)
    }
}

impl<I> Encoder<Vec<u8>> for Codec<I> {
    type Error = io::Error;
    fn encode(&mut self, encoded: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.reserve(encoded.len());
        dst.put(&encoded[..]);
        Ok(())
    }
}

/// The main handle to our end of the mirrord-protocol connection.
/// `Type` is either [`Client`] or [`Agent`].
///
/// Implements message queueing and almost-fair scheduling. Use
/// [`Self::tx_handle`] to obtain a handle for sending messages.
/// Messages sent with different send handles are *not* guaranteed to
/// be delivered in order. To preserve message delivery order, send
/// all relevant messages through a single [`TxHandle`], or clones
/// derived from it.
///
/// The impl works by randomly picking a message from one of the
/// nonempty queues and sending it over the wire. Future versions will
/// use message chunking to fairly split the available bandwidth
/// between all queues (as messages can vary in size).
pub struct Connection<Type: ProtocolEndpoint> {
    rx: mpsc::Receiver<Type::InMsg>,
    shared_state: Arc<SharedState<Type>>,
}

impl<Type: ProtocolEndpoint> Connection<Type> {
    /// Create a new connection running over a byte stream, i.e.
    /// `AsyncRead` + `AsyncWrite`.
    pub async fn from_stream<IO>(inner: IO) -> Result<Self, ProtocolError>
    where
        IO: AsyncIO,
    {
        let framed = Framed::new(inner, Codec::<Type::InMsg>(PhantomData));

        let (inbound_tx, inbound_rx) = mpsc::channel(64);

        let shared_state = Arc::new(SharedState::new(inbound_tx.downgrade(), None));

        tokio::spawn(io_task::<_, Type>(
            framed,
            Arc::clone(&shared_state),
            inbound_tx,
        ));

        Ok(Self {
            rx: inbound_rx,
            shared_state,
        })
    }

    /// Create a new connection, running over a `Sink` + `Stream` of `Vec<u8>`s.
    /// Used for connecting to the operator over a websocket connection.
    pub async fn from_channel<C, Filter>(
        channel: C,
        filter: Option<Filter>,
    ) -> Result<Self, ProtocolError>
    where
        C: Transport<Vec<u8>, Vec<u8>>,
        Filter: Fn(Type::OutMsg) -> Either<Type::OutMsg, Type::InMsg> + Send + Sync + 'static,
        C::Error: From<DecodeError> + std::error::Error + Send + 'static,
    {
        let framed = channel.map(|msg| {
            msg.and_then(|e| {
                bincode::decode_from_slice::<Type::InMsg, _>(&e, bincode::config::standard())
                    .map(|(msg, _)| msg)
                    .map_err(<C::Error as From<DecodeError>>::from)
            })
        });

        let (inbound_tx, inbound_rx) = mpsc::channel(64);

        let shared_state = Arc::new(SharedState::new(
            inbound_tx.downgrade(),
            filter.map(|f| Box::new(f) as _),
        ));

        tokio::spawn(io_task::<_, Type>(
            framed,
            Arc::clone(&shared_state),
            inbound_tx,
        ));

        Ok(Self {
            rx: inbound_rx,
            shared_state,
        })
    }

    /// Create a dummy instance, mainly used for tests.
    pub fn dummy() -> (Self, mpsc::Sender<Type::InMsg>, ConnectionOutput<Type>) {
        let (inbound_tx, inbound_rx) = mpsc::channel(32);

        let shared_state = Arc::new(SharedState::new(inbound_tx.downgrade(), None));

        let connection = Connection {
            rx: inbound_rx,
            shared_state: Arc::clone(&shared_state),
        };

        (
            connection,
            inbound_tx,
            ConnectionOutput::<Type>(shared_state),
        )
    }

    /// Receive the next message.
    ///
    /// Cancel safe.
    #[inline]
    pub async fn recv(&mut self) -> Option<Type::InMsg> {
        self.rx.recv().await
    }

    /// Poll the next message.
    #[inline]
    pub fn poll_recv(&mut self, cx: &mut Context) -> Poll<Option<Type::InMsg>> {
        self.rx.poll_recv(cx)
    }

    /// Send a message.
    ///
    /// Note that all messages sent with this function
    /// will be put in a single queue, and thus be delivered
    /// sequentially. Use [`Self::tx_handle`] for queueing messages
    /// independently.
    #[inline]
    pub async fn send(&self, msg: Type::OutMsg) {
        self.shared_state.push(QueueId(0), msg).await
    }

    /// Returns a send handle to a *new* queue.
    #[inline]
    pub fn tx_handle(&self) -> TxHandle<Type> {
        self.shared_state.clone().new_queue()
    }

    #[inline]
    pub fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }
}

impl<Type: ProtocolEndpoint> Drop for Connection<Type> {
    fn drop(&mut self) {
        self.shared_state.cancel.cancel();
    }
}

/// The "other end" of a `Connection`. Used for pulling out messages
/// sent through it. Used only for testing.
pub struct ConnectionOutput<Type: ProtocolEndpoint>(Arc<SharedState<Type>>);

impl<Type: ProtocolEndpoint> ConnectionOutput<Type>
where
    Type::OutMsg: bincode::Decode<()>,
{
    pub async fn next(&self) -> Option<Type::OutMsg> {
        bincode::decode_from_slice(&self.0.next().await, bincode::config::standard())
            .ok()
            .map(|e| e.0)
    }
}

#[instrument(level = Level::TRACE, name = "mirrord_protocol_io_task", skip_all)]
async fn io_task<Channel, Type>(
    framed: Channel,
    queues: Arc<SharedState<Type>>,
    tx: mpsc::Sender<Type::InMsg>,
) where
    Type: ProtocolEndpoint,
    Channel: Transport<Type::InMsg, Vec<u8>>,
    Channel::Error: std::error::Error + Send,
{
    pin!(framed);
    loop {
        select! {
            // When >=2 futures complete simultaneously, poll them in
            // the order they are declared here. We want everything to
            // be pulled out of the queues and processed before
            // reacting to the cancellation token, otherwise we will
            // drop messages.
            biased;

            to_send = queues.next() => {
                if let Err(error) = framed.send(to_send).await {
                    tracing::error!(?error, "failed to send message");
                    break;
                }
            }
            received = framed.next() => {
                match received {
                    None => {
                        tracing::info!("no more messages, exiting task");
                        break;
                    }
                    Some(Err(error)) => {
                        tracing::error!(?error, "failed to receive message");
                        break;
                    }
                    Some(Ok(msg)) => {
                        if let Err(error) = tx.send(msg).await {
                            tracing::info!(?error, "io task channel closed");
                            break;
                        }
                    },
                }
            }
            _ = queues.cancel.cancelled() => {
                tracing::info!("Cancellation token triggered, shutting down after flushing all remaining messages");
                break;
            }
        }
    }

    let _ = framed.close().await;
}

#[derive(Debug, Default)]
struct OutQueue {
    messages: VecDeque<Vec<u8>>,
    used_bytes: usize,

    free: Arc<Notify>,
}

type FilterFn<I, O> = dyn Fn(O) -> Either<O, I> + Send + Sync;

#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
struct QueueId(usize);

struct Queues {
    queues: HashMap<QueueId, OutQueue>,
    ready: Vec<QueueId>,
}

struct SharedState<Type: ProtocolEndpoint> {
    queues: Mutex<Queues>,
    nonempty: Arc<Notify>,

    /// Used for injecting "fake" incoming messages. It's a weak
    /// sender because the "main" strong (i.e. normal) sender is owned
    /// by the [`io_task`], and we want the channel to close once it
    /// exits. There's no point injecting messages anyway if the IO
    /// task is dead.
    in_tx: mpsc::WeakSender<Type::InMsg>,

    /// Used for doing a sort of filter_map and message faking. If
    /// Some, all outgoing messages are passed to this function, and
    /// the return value is either injected as an incoming message
    /// (left), or enqueued for sending (right).
    out_filter: Option<Box<FilterFn<Type::InMsg, Type::OutMsg>>>,

    next_queue_id: AtomicUsize,

    /// Used for telling the io task to shut down.
    cancel: CancellationToken,
}

impl<Type: ProtocolEndpoint> fmt::Debug for SharedState<Type> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedState")
            .field("address", &(&self as *const _))
            .field("in_tx_strong_count", &self.in_tx.strong_count())
            .field("next_queue_id", &self.next_queue_id)
            .finish()
    }
}

impl<Type: ProtocolEndpoint> SharedState<Type> {
    const MAX_CAPACITY: usize = 1024 * 16;
    fn new(
        in_tx: mpsc::WeakSender<Type::InMsg>,
        out_filter: Option<Box<FilterFn<Type::InMsg, Type::OutMsg>>>,
    ) -> Self {
        Self {
            queues: Mutex::new(Queues {
                queues: HashMap::new(),
                ready: Vec::new(),
            }),
            nonempty: Arc::new(Notify::new()),
            in_tx,
            out_filter,
            // 0 is reserved for the Connection struct
            next_queue_id: 1.into(),
            cancel: CancellationToken::new(),
        }
    }

    /// Try to push a new message into the queue with id `queue_id`,
    /// creating it if it doesn't exist. If the queue is full, return
    /// the message and an `OwnedNotified` that will resolve when the
    /// queue has free capacity.
    fn try_push(
        &self,
        queue_id: QueueId,
        encoded: Vec<u8>,
    ) -> Result<(), (Vec<u8>, OwnedNotified)> {
        let mut lock = self.queues.lock().unwrap();

        // Garbage-collect unused queues
        if lock.queues.len() > 8 && lock.ready.len() < lock.queues.len() / 3 {
            // We can remove all empty ones because we know they wont be in `ready`
            lock.queues.retain(|_, v| !v.messages.is_empty());
            lock.queues.shrink_to_fit();
        }

        let queue = lock.queues.entry(queue_id).or_default();

        if queue.used_bytes > Self::MAX_CAPACITY {
            return Err((encoded, queue.free.clone().notified_owned()));
        }

        queue.used_bytes += encoded.len();
        queue.messages.push_back(encoded);

        if queue.messages.len() == 1 {
            // .len() > 1 implies that it was already in `ready`
            lock.ready.push(queue_id);
        }

        drop(lock);

        self.nonempty.notify_one();

        Ok(())
    }

    /// Push a message into the given queue, creating it if necessary.
    /// Will wait for the queue to free up if necessary, resolves only
    /// when the message has been successfully pushed.
    async fn push(&self, id: QueueId, mut msg: Type::OutMsg) {
        if let Some(filter) = &self.out_filter {
            match filter(msg) {
                Either::Left(out) => msg = out,
                Either::Right(inj) => {
                    match self.in_tx.upgrade() {
                        Some(tx) => {
                            let _ = tx.send(inj).await;
                        }
                        None => {
                            tracing::warn!(
                                "Cannot inject response to filtered message because the IO task has closed. Dropping."
                            )
                        }
                    }
                    return;
                }
            }
        }

        let mut encoded = bincode::encode_to_vec(msg, bincode::config::standard()).unwrap();

        loop {
            match self.try_push(id, encoded) {
                Ok(()) => break,
                Err((r, notify)) => {
                    encoded = r;
                    notify.await;
                }
            }
        }
    }

    /// Check for enqueued messages and return one from a
    /// randomly-picked nonempty queue.
    fn poll_next(&self) -> Option<Vec<u8>> {
        let mut lock = self.queues.lock().unwrap();

        // If `ready` is empty then we have nothing to do.
        let key_idx = (0..lock.ready.len()).choose(&mut rand::rng())?;
        let key = *lock.ready.get(key_idx).unwrap();

        let queue = lock
            .queues
            .get_mut(&key)
            .expect("key was in Queues::ready but the corresponding queue was not found");

        let next = queue
            .messages
            .pop_front()
            .expect("key was in Queues::ready but the corresponding queue was empty");

        let was_full = queue.used_bytes >= Self::MAX_CAPACITY;

        queue.used_bytes -= next.len();

        if was_full && queue.used_bytes < Self::MAX_CAPACITY {
            queue.free.notify_waiters();
        }

        if queue.messages.is_empty() {
            lock.ready.swap_remove(key_idx);
        }

        Some(next)
    }

    /// Wait for a new message to be enqueued and return it.
    async fn next(&self) -> Vec<u8> {
        loop {
            match self.poll_next() {
                Some(msg) => break msg,
                None => {
                    self.nonempty.notified().await;
                }
            }
        }
    }

    /// Return a new [`TxHandle`] to a new independent queue.
    ///
    /// The queue is actually created lazily and may be
    /// garbage-collected and recreated later on, but that process is
    /// transparent to users of this function.
    fn new_queue(self: Arc<Self>) -> TxHandle<Type> {
        let next = self.next_queue_id.fetch_add(1, Ordering::Relaxed);
        if next > usize::MAX / 2 {
            panic!("Too many QueueIds generated");
        }

        TxHandle {
            shared_state: self,
            id: QueueId(next),
        }
    }
}

/// A handle to a queue for outbound messages.
///
/// Cloning this struct returns a handle to the same queue. To create
/// a new queue, use [`Self::another`].
#[derive(Clone)]
pub struct TxHandle<Type: ProtocolEndpoint> {
    shared_state: Arc<SharedState<Type>>,
    id: QueueId,
}

impl<Type: ProtocolEndpoint> PartialEq for TxHandle<Type> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared_state, &other.shared_state) && self.id == other.id
    }
}
impl<Type: ProtocolEndpoint> Eq for TxHandle<Type> {}

impl<Type: ProtocolEndpoint> fmt::Debug for TxHandle<Type> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxHandle")
            .field("shared_state", &self.shared_state)
            .field("id", &self.id)
            .finish()
    }
}

impl<Type: ProtocolEndpoint> TxHandle<Type> {
    pub async fn send(&self, msg: Type::OutMsg) {
        self.shared_state.push(self.id, msg).await
    }

    /// Returns a `TxHandle` to a *new* queue, independent from this
    /// one.
    pub fn another(&self) -> Self {
        self.shared_state.clone().new_queue()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bincode::{Decode, Encode};
    use rand::{Rng, seq::IndexedRandom};
    use rstest::rstest;
    use tokio::time::timeout;

    use super::*;

    #[derive(Clone, Debug, Encode, Decode, PartialEq, Eq)]
    struct Message {
        from: u32,
        payload: u64,
    }

    impl Message {
        fn new(from: u32) -> Self {
            Self {
                from,
                payload: rand::rng().random(),
            }
        }
    }

    #[derive(Clone)]
    struct Test;
    impl ProtocolEndpoint for Test {
        type InMsg = Message;
        type OutMsg = Message;
    }

    #[tokio::test]
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    async fn preserves_order() {
        let (connection, _inbound_tx, output) = Connection::<Test>::dummy();

        let msg_count_per_queue = 200;
        let num_queues = 20;
        let handles_per_queue = 5;

        let sequences: Vec<Vec<Message>> = (0..num_queues)
            .map(|n| (0..msg_count_per_queue).map(|_| Message::new(n)).collect())
            .collect();

        let mut tasks = Vec::new();

        for sequence in sequences.clone() {
            let mut handles = vec![connection.tx_handle()];
            (0..handles_per_queue).for_each(|_| handles.push(handles.first().unwrap().clone()));

            tasks.push(tokio::spawn(async move {
                for msg in sequence {
                    let handle = handles.choose(&mut rand::rng()).unwrap();
                    handle.send(msg).await;
                }
            }));
        }

        // Wait for all sends to complete
        for task in tasks {
            task.await.unwrap();
        }

        // Collect all received messages and verify they're in a valid order.
        let mut sequences: Vec<_> = sequences.into_iter().map(|v| v.into_iter()).collect();

        loop {
            let Ok(msg) = timeout(Duration::from_millis(100), output.next()).await else {
                break;
            };

            let msg = msg.expect("decoding message failed");

            let sequence = sequences
                .get_mut(msg.from as usize)
                .expect("Received a message with an invalid `from` field");

            assert_eq!(sequence.next(), Some(msg));
        }

        // Assert we received all the messages with none missing
        for mut seq in sequences {
            assert_eq!(seq.next(), None);
        }
    }
}
