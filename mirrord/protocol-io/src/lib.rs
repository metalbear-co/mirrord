use std::{
    collections::{HashMap, VecDeque},
    fmt,
    io::{self},
    marker::PhantomData,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use bincode::error::DecodeError;
use bytes::{BufMut, BytesMut};
use futures::{Sink, SinkExt, Stream, StreamExt, future::Either};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, ProtocolCodec,
    queueing::{QueueId, Queueable},
};
use rand::seq::IteratorRandom;
use tokio::{
    pin, select,
    sync::{Notify, futures::OwnedNotified, mpsc},
};
use tracing::instrument;

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

pub enum Mode {
    Legacy,
    Chunked,
}

pub trait ProtocolEndpoint: 'static + Sized + Clone {
    type InMsg: bincode::Decode<()> + Send + fmt::Debug;
    type OutMsg: bincode::Encode + Send + Queueable + fmt::Debug;
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
    #[error("unexpected peer message")]
    UnexpectedPeerMessage,
}

#[derive(Clone)]
pub struct Client;
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

pub struct Connection<Type: ProtocolEndpoint> {
    rx: mpsc::Receiver<Type::InMsg>,
    tx_handle: TxHandle<Type>,
}

impl<Type: ProtocolEndpoint> Connection<Type> {
    pub async fn from_stream<IO>(inner: IO) -> Result<Self, ProtocolError>
    where
        IO: AsyncIO,
    {
        let framed = Framed::new(inner, Codec::<Type::InMsg>(PhantomData));

        let (inbound_tx, inbound_rx) = mpsc::channel(64);

        let out_queues = Arc::new(SharedState::new(inbound_tx.clone(), None));

        let tx_handle = TxHandle(out_queues.clone());

        tokio::spawn(io_task::<_, Type>(framed, out_queues, inbound_tx));

        Ok(Self {
            rx: inbound_rx,
            tx_handle,
        })
    }

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

        let out_queues = Arc::new(SharedState::new(
            inbound_tx.clone(),
            filter.map(|f| Box::new(f) as _),
        ));

        let tx_handle = TxHandle::<Type>(out_queues.clone());
        tokio::spawn(io_task::<_, Type>(framed, out_queues, inbound_tx));

        Ok(Self {
            rx: inbound_rx,
            tx_handle,
        })
    }

    pub fn dummy() -> (Self, mpsc::Sender<Type::InMsg>, ConnectionOutput<Type>) {
        let (inbound_tx, inbound_rx) = mpsc::channel(32);

        let out_queues = Arc::new(SharedState::new(inbound_tx.clone(), None));

        let tx_handle = TxHandle(out_queues.clone());

        let connection = Connection {
            rx: inbound_rx,
            tx_handle,
        };

        (connection, inbound_tx, ConnectionOutput::<Type>(out_queues))
    }

    /// Replaces the internal send queue of this connection with the
    /// one provided. Used to preserve and carry over queued messages
    /// between reconnections.
    pub fn replace_queue(&mut self, tx_handle: TxHandle<Type>) {
        self.tx_handle = tx_handle;
    }

    #[inline]
    pub async fn recv(&mut self) -> Option<Type::InMsg> {
        self.rx.recv().await
    }

    #[inline]
    pub fn poll_recv(&mut self, cx: &mut Context) -> Poll<Option<Type::InMsg>> {
        self.rx.poll_recv(cx)
    }

    #[inline]
    pub async fn send(&self, msg: Type::OutMsg) {
        self.tx_handle.send(msg).await
    }

    #[inline]
    pub fn tx_handle(&self) -> TxHandle<Type> {
        self.tx_handle.clone()
    }

    #[inline]
    pub fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }
}

// For tests mostly
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

#[instrument(skip_all)]
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
            to_send = queues.next() => {
                if let Err(err) = framed.send(to_send).await {
                    tracing::error!(?err, "failed to send message");
                    break;
                }
            }
            received = framed.next() => {
                match received {
                    None => {
                        tracing::info!("no more messages, exiting task");
                        break;
                    }
                    Some(Err(err)) => {
                        tracing::error!(?err, "failed to receive message");
                        break;
                    }
                    Some(Ok(e)) => {
                        if let Err(e) = tx.send(e).await {
                            tracing::info!(?e, "io task channel closed");
                            break;
                        }
                    },
                }
            }
        }
    }

    let _ = framed.close().await;
}

#[derive(Default)]
struct OutQueue {
    messages: VecDeque<Vec<u8>>,
    used_bytes: usize,

    free: Arc<Notify>,
}

type FilterFn<I, O> = dyn Fn(O) -> Either<O, I> + Send + Sync;

struct Queues<Type: ProtocolEndpoint> {
    queues: HashMap<QueueId<Type::OutMsg>, OutQueue>,
    ready: Vec<QueueId<Type::OutMsg>>,
}

struct SharedState<Type: ProtocolEndpoint> {
    queues: Mutex<Queues<Type>>,
    nonempty: Arc<Notify>,

    /// Used for injecting "fake" incoming messages.
    in_tx: mpsc::Sender<Type::InMsg>,

    /// Used for doing a sort of filter_map and message faking. If
    /// Some, all outgoing messages are passed to this function, and
    /// the return value is either injected as an incoming message
    /// (left), or enqueued for sending (right).
    out_filter: Option<Box<FilterFn<Type::InMsg, Type::OutMsg>>>,
}

impl<Type: ProtocolEndpoint> SharedState<Type> {
    const MAX_CAPACITY: usize = 1024 * 16;
    fn new(
        in_tx: mpsc::Sender<Type::InMsg>,
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
        }
    }

    fn try_push(
        &self,
        queue_id: QueueId<Type::OutMsg>,
        encoded: Vec<u8>,
    ) -> Result<(), (Vec<u8>, OwnedNotified)> {
        let mut lock = self.queues.lock().unwrap();

        // Garbage-collect unused queues
        if lock.queues.len() > 8 && lock.ready.len() < lock.queues.len() / 3 {
			// We can remove all empty ones because we know they wont be in `ready`
            lock.queues.retain(|_, v| !v.messages.is_empty());
        }

        let queue = lock.queues.entry(queue_id.clone()).or_default();

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

    fn poll_next(&self) -> Option<Vec<u8>> {
        let mut lock = self.queues.lock().unwrap();

		// If `ready` is empty then we have nothing to do.
        let key_idx = (0..lock.ready.len()).choose(&mut rand::rng())?;
        let key = lock.ready[key_idx].clone();

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
}

#[derive(Clone)]
pub struct TxHandle<Type: ProtocolEndpoint>(Arc<SharedState<Type>>);

impl<Type: ProtocolEndpoint> TxHandle<Type> {
    pub async fn send(&self, mut msg: Type::OutMsg) {
        if let Some(filter) = &self.0.out_filter {
            match filter(msg) {
                Either::Left(out) => msg = out,
                Either::Right(inj) => {
                    let _ = self.0.in_tx.send(inj).await;
                    return;
                }
            }
        }

        let queue_id = msg.queue_id();
        let mut encoded = bincode::encode_to_vec(msg, bincode::config::standard()).unwrap();

        loop {
            match self.0.try_push(queue_id.clone(), encoded) {
                Ok(()) => break,
                Err((r, notify)) => {
                    encoded = r;
                    notify.await;
                }
            }
        }
    }
}
