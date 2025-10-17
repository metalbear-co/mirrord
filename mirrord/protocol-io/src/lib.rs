use std::{
    collections::{HashMap, VecDeque},
    fmt,
    io::{self},
    marker::PhantomData,
    sync::{Arc, Mutex},
};

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use bytes::{BufMut, Bytes, BytesMut};
use futures::{Sink, SinkExt, Stream, StreamExt};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, ProtocolCodec,
    queueing::{QueueId, Queueable},
};
use rand::seq::IteratorRandom;
use tokio::{
    select,
    sync::{Notify, futures::OwnedNotified, mpsc},
};
use tracing::instrument;

pub trait AsyncIO: AsyncWrite + AsyncRead + Send + Unpin + 'static {}
impl<T: AsyncWrite + AsyncRead + Send + Unpin + 'static> AsyncIO for T {}

pub trait Transport<I, O>:
    Sink<O, Error = io::Error> + Stream<Item = Result<I, io::Error>> + Send + Unpin + 'static
{
}

impl<T, I, O> Transport<I, O> for T where
    T: Sink<O, Error = io::Error> + Stream<Item = Result<I, io::Error>> + Send + Unpin + 'static
{
}

pub enum Mode {
    Legacy,
    Chunked,
}

pub trait ProtocolEndpoint: 'static + Sized {
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

pub struct Client;
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
    pub rx: mpsc::Receiver<Type::InMsg>,
    tx_handle: TxHandle<Type>,
}

impl<Type: ProtocolEndpoint> Connection<Type> {
    pub async fn from_stream<IO: AsyncIO>(inner: IO) -> Result<Self, ProtocolError> {
        let framed = Framed::new(inner, Codec::<Type::InMsg>(PhantomData));

        let (inbound_tx, inbound_rx) = mpsc::channel(64);

        let out_queues = Arc::new(OutQueues {
            queues: Mutex::new(HashMap::new()),
            nonempty: Arc::new(Notify::new()),
        });

        let tx_handle = TxHandle(out_queues.clone());

        tokio::spawn(io_task::<_, Type>(framed, out_queues, inbound_tx));

        Ok(Self {
            rx: inbound_rx,
            tx_handle,
        })
    }

    pub async fn from_channel<C>(channel: C) -> Result<Self, ProtocolError>
    where
        C: Transport<Vec<u8>, Vec<u8>>,
    {
        let framed = channel.map(|msg| {
            msg.and_then(|e| {
                bincode::decode_from_slice::<Type::InMsg, _>(&e, bincode::config::standard())
                    .map(|(msg, _)| msg)
                    .map_err(io::Error::other)
            })
        });

        let (inbound_tx, inbound_rx) = mpsc::channel(64);

        let out_queues = Arc::new(OutQueues {
            queues: Mutex::new(HashMap::new()),
            nonempty: Arc::new(Notify::new()),
        });

        let tx_handle = TxHandle::<Type>(out_queues.clone());
        tokio::spawn(io_task::<_, Type>(framed, out_queues, inbound_tx));

        Ok(Self {
            rx: inbound_rx,
            tx_handle,
        })
    }

    pub fn dummy() -> (
        Self,
        mpsc::Sender<Type::InMsg>,
        ConnectionOutput<Type::OutMsg>,
    ) {
        let (in_tx, in_rx) = mpsc::channel(32);

        let out_queues = Arc::new(OutQueues {
            queues: Mutex::new(HashMap::new()),
            nonempty: Arc::new(Notify::new()),
        });

        let tx_handle = TxHandle(out_queues.clone());

        let connection = Connection {
            rx: in_rx,
            tx_handle,
        };

        (connection, in_tx, ConnectionOutput(out_queues))
    }

    #[inline]
    pub async fn recv(&mut self) -> Option<Type::InMsg> {
        self.rx.recv().await
    }

    #[inline]
    pub async fn send(&self, msg: Type::OutMsg) {
        self.tx_handle.send(msg).await
    }

    #[inline]
    pub fn tx_handle(&self) -> TxHandle<Type> {
        self.tx_handle.clone()
    }
}

// For tests mostly
pub struct ConnectionOutput<T>(Arc<OutQueues<T>>);

impl<T: bincode::Decode<()>> ConnectionOutput<T> {
    pub async fn next(&self) -> Option<T> {
        bincode::decode_from_slice(&self.0.next().await, bincode::config::standard())
            .ok()
            .map(|e| e.0)
    }
}

#[instrument(skip_all)]
async fn io_task<Channel, Type>(
    mut framed: Channel,
    queues: Arc<OutQueues<Type::OutMsg>>,
    tx: mpsc::Sender<Type::InMsg>,
) where
    Type: ProtocolEndpoint,
    Channel: Transport<Type::InMsg, Vec<u8>>,
{
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

struct OutQueues<T> {
    queues: Mutex<HashMap<QueueId<T>, OutQueue>>,
    nonempty: Arc<Notify>,
}

impl<T> OutQueues<T> {
    const MAX_CAPACITY: usize = 1024 * 16;
    fn try_push(
        &self,
        queue_id: QueueId<T>,
        encoded: Vec<u8>,
    ) -> Result<(), (Vec<u8>, OwnedNotified)> {
        let mut lock = self.queues.lock().unwrap();
        let queue = lock.entry(queue_id.clone()).or_default();

        if queue.used_bytes > Self::MAX_CAPACITY {
            return Err((encoded, queue.free.clone().notified_owned()));
        }

        queue.used_bytes += encoded.len();
        queue.messages.push_back(encoded);

        drop(lock);

        self.nonempty.notify_one();

        Ok(())
    }

    fn poll_next(&self) -> Option<Vec<u8>> {
        let mut lock = self.queues.lock().unwrap();

        let (key, queue) = lock
            .iter_mut()
            .choose(&mut rand::rng())
            .map(|(k, v)| (k.clone(), v))?;

        let Some(next) = queue.messages.pop_front() else {
            // Shouldn't really happen
            lock.remove(&key);
            return None;
        };

        let was_full = queue.used_bytes >= Self::MAX_CAPACITY;

        queue.used_bytes -= next.len();

        if was_full && queue.used_bytes < Self::MAX_CAPACITY {
            queue.free.notify_waiters();
        }

        if queue.messages.len() == 0 {
            lock.remove(&key);
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

pub struct TxHandle<Type: ProtocolEndpoint>(Arc<OutQueues<Type::OutMsg>>);
impl<Type: ProtocolEndpoint> Clone for TxHandle<Type> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<Type: ProtocolEndpoint> TxHandle<Type> {
    pub async fn send(&self, msg: Type::OutMsg) {
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
