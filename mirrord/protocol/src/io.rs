use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    io::{self},
    mem::{Discriminant, discriminant},
    sync::{Arc, Mutex},
};

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use bincode::{Decode, Encode, error::DecodeError};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::{SinkExt, StreamExt, stream::SplitSink};
use rand::seq::IteratorRandom;
use semver::Version;
use tokio::{
    select,
    sync::{Notify, mpsc},
};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use crate::{
    ClientCodec, ClientMessage, ConnectionId, DaemonCodec, DaemonMessage, Payload, ToPayload,
    VERSION, file::CHUNKED_PROTOCOL_VERSION,
};

pub trait AsyncIO: AsyncWrite + AsyncRead + Send + Unpin + 'static {}
impl<T: AsyncWrite + AsyncRead + Send + Unpin + 'static> AsyncIO for T {}

pub enum Mode {
    Legacy,
    Chunked,
}

/// A trait implemented on message types that dictates how queueing
/// should work for different message types.
pub trait Queueable: Sized {
    /// Returns the queue id that should be used for this message.
    /// Messages with the same queue id will end up in the same queue
    /// and thus be processed sequentially. Messages with different
    /// queue ids will end up in different queues and may be processed
    /// out of order. This is used for fair scheduling between
    /// different logical data streams, e.g. tcp data packets will use
    /// their connection id as the queueid. Most other message types
    /// should use the enum discriminant as the queue id.
    fn queue_id(&self) -> QueueId<Self>;
}

pub trait ProtocolEndpoint: 'static + Sized {
    type InMsg: bincode::Decode<()> + Send;
    type OutMsg: bincode::Encode + Send + Queueable;
    type Codec: Encoder<Self::OutMsg, Error = io::Error>
        + Decoder<Item = Self::InMsg, Error = io::Error>
        + Default
        + Send;

    fn negotiate<IO: AsyncIO>(
        io: &mut Framed<IO, Self::Codec>,
    ) -> impl Future<Output = Result<(Mode, Version), ProtocolError>> + Send;
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
    type Codec = ClientCodec;

    async fn negotiate<IO: AsyncIO>(
        io: &mut Framed<IO, Self::Codec>,
    ) -> Result<(Mode, Version), ProtocolError> {
        io.send(ClientMessage::SwitchProtocolVersion(VERSION.clone()))
            .await?;

        let version = match io.next().await {
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unable to receive agent response",
                )
                .into());
            }
            Some(Err(err)) => return Err(err.into()),
            Some(Ok(DaemonMessage::SwitchProtocolVersionResponse(version))) => version,
            Some(Ok(_)) => {
                return Err(ProtocolError::UnexpectedPeerMessage);
            }
        };

        let mode = if CHUNKED_PROTOCOL_VERSION.matches(&version) {
            Mode::Chunked
        } else {
            Mode::Legacy
        };

        Ok((mode, version))
    }
}

impl ProtocolEndpoint for Agent {
    type InMsg = ClientMessage;
    type OutMsg = DaemonMessage;
    type Codec = DaemonCodec;

    async fn negotiate<IO: AsyncIO>(
        io: &mut Framed<IO, Self::Codec>,
    ) -> Result<(Mode, Version), ProtocolError> {
        // REVIEW: Handle non-switchprotocolversion first messages
        // REVIEW: Fix multiple switchprotocolversions taking place
        let client_version = match io.next().await {
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unable to receive client switch protocol requst",
                )
                .into());
            }
            Some(Err(err)) => return Err(err.into()),
            Some(Ok(ClientMessage::SwitchProtocolVersion(version))) => version,
            Some(Ok(_)) => return Err(ProtocolError::UnexpectedPeerMessage),
        };

        let version = Ord::min(&client_version, &VERSION).clone();

        io.send(DaemonMessage::SwitchProtocolVersionResponse(
            version.clone(),
        ))
        .await?;

        let mode = if CHUNKED_PROTOCOL_VERSION.matches(&version) {
            Mode::Chunked
        } else {
            Mode::Legacy
        };

        Ok((mode, version))
    }
}

pub struct Connection<Type: ProtocolEndpoint> {
    pub tx: mpsc::Sender<Type::OutMsg>,
    pub rx: mpsc::Receiver<Type::InMsg>,
    pub version: Version,
}

impl<Type: ProtocolEndpoint> Connection<Type> {
    pub async fn new<IO: AsyncIO>(inner: IO) -> Result<Self, ProtocolError> {
        let mut framed = Framed::new(inner, Type::Codec::default());
        let (mode, version) = Type::negotiate(&mut framed).await?;
        let (inbound_tx, inbound_rx) = mpsc::channel(32);
        let (outbound_tx, outbound_rx) = mpsc::channel(32);

        match mode {
            Mode::Legacy => {
                tokio::spawn(io_task_legacy::<IO, Type>(framed, outbound_rx, inbound_tx))
            }
            Mode::Chunked => tokio::spawn(io_task_chunked::<IO, Type>(
                framed.into_parts().io,
                outbound_rx,
                inbound_tx,
            )),
        };

        Ok(Self {
            tx: outbound_tx,
            rx: inbound_rx,
            version,
        })
    }

    pub async fn send(
        &self,
        msg: Type::OutMsg,
        /* REVIEW return type */
    ) -> Result<(), mpsc::error::SendError<Type::OutMsg>> {
        self.tx.send(msg).await
    }

    pub async fn recv(
        &mut self,
        /* REVIEW return type */
    ) -> Option<Type::InMsg> {
        self.rx.recv().await
    }
}

#[instrument(skip_all)]
async fn io_task_legacy<IO: AsyncIO, Type: ProtocolEndpoint>(
    mut framed: Framed<IO, Type::Codec>,
    mut rx: mpsc::Receiver<Type::OutMsg>,
    tx: mpsc::Sender<Type::InMsg>,
) {
    loop {
        select! {
            to_send = rx.recv() => {
                let Some(to_send) = to_send else {
                    tracing::info!("io task rx channel closed");
                    break;
                };
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

type MessageId = u16;
type PayloadLen = u32;

#[derive(Debug, Encode, Decode)]
struct Chunk {
    id: MessageId,
    payload: Payload,
}

impl Chunk {
    const ID_SIZE: usize = size_of::<MessageId>();
    const PAYLOAD_LEN_SIZE: usize = size_of::<PayloadLen>();

    const HEADER_SIZE: usize = Self::ID_SIZE + Self::PAYLOAD_LEN_SIZE;
}

struct OutMessage {
    encoded: Bytes,
    next_chunk_offset: u32,
    id: MessageId,
}

impl OutMessage {
    fn encode<T: bincode::Encode>(message: T, id: MessageId) -> Self {
        let encoded: Bytes = bincode::encode_to_vec(message, bincode::config::standard())
            .expect("failed to encode message")
            .into();

        Self {
            encoded,
            next_chunk_offset: 0,
            id,
        }
    }
}

/// REVIEW make this configurable
const CHUNK_SIZE: u32 = 1024 * 256;

impl Iterator for OutMessage {
    type Item = Chunk;

    fn next(&mut self) -> Option<Self::Item> {
        let size = Ord::min(
            CHUNK_SIZE,
            self.encoded.len() as PayloadLen - self.next_chunk_offset,
        );

        if size == 0 {
            return None;
        }

        let payload = self
            .encoded
            .slice(self.next_chunk_offset as usize..(self.next_chunk_offset + size) as usize)
            .into();

        self.next_chunk_offset += size;

        Some(Chunk {
            id: self.id,
            payload,
        })
    }
}

#[derive(Default)]
struct InBuffers {
    partially_received: HashMap<MessageId, Vec<u8>>,
}

struct SimpleChunkCodec;

impl Encoder<Chunk> for SimpleChunkCodec {
    type Error = io::Error;

    fn encode(&mut self, chunk: Chunk, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.put_u16(chunk.id);
        dst.put_u32(chunk.payload.len() as PayloadLen);
        dst.put(chunk.payload.0);
        Ok(())
    }
}

impl Decoder for SimpleChunkCodec {
    type Item = Chunk;

    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < Chunk::ID_SIZE + Chunk::PAYLOAD_LEN_SIZE + 1 {
            return Ok(None);
        }

        let expected_len = PayloadLen::from_be_bytes(
            src[Chunk::ID_SIZE..Chunk::ID_SIZE + Chunk::PAYLOAD_LEN_SIZE]
                .try_into()
                .unwrap(),
        ) as usize;

        let actual_payload_size = src.len() - Chunk::HEADER_SIZE;

        if expected_len > actual_payload_size {
            src.reserve(expected_len - actual_payload_size);
            return Ok(None);
        }

        let id = MessageId::from_be_bytes(src[0..Chunk::ID_SIZE].try_into().unwrap());
        let payload = (&src[Chunk::HEADER_SIZE..Chunk::HEADER_SIZE + expected_len]).to_payload();

        src.advance(expected_len + Chunk::HEADER_SIZE);

        Ok(Some(Chunk { id, payload }))
    }
}

pub enum QueueId<T> {
    /// For messages which have all their instances in a single queue
    Normal(Discriminant<T>),

    /// For tcp data messages (we want to split bandwidth equally between tcp connections)
    Tcp(ConnectionId),
}

// Need these because derives add unnecessary bounds on T
impl<T> Clone for QueueId<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Normal(arg0) => Self::Normal(arg0.clone()),
            Self::Tcp(arg0) => Self::Tcp(arg0.clone()),
        }
    }
}
impl<T> Hash for QueueId<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        discriminant(self).hash(state);
        match self {
            QueueId::Normal(discriminant) => discriminant.hash(state),
            QueueId::Tcp(id) => id.hash(state),
        }
    }
}

impl<T> PartialEq for QueueId<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Normal(l0), Self::Normal(r0)) => l0 == r0,
            (Self::Tcp(l0), Self::Tcp(r0)) => l0 == r0,
            _ => false,
        }
    }
}
impl<T> Eq for QueueId<T> {}

#[derive(Default)]
struct OutQueue {
    messages: VecDeque<OutMessage>,
}

struct OutQueues<T> {
    queues: Mutex<HashMap<QueueId<T>, OutQueue>>,
    can_send: Notify,
}

#[instrument(skip_all)]
async fn io_task_chunked<IO: AsyncIO, Type: ProtocolEndpoint>(
    io: IO,
    mut rx: mpsc::Receiver<Type::OutMsg>,
    tx: mpsc::Sender<Type::InMsg>,
) {
    let out_queues = Arc::new(OutQueues {
        queues: Mutex::new(HashMap::new()),
        can_send: Notify::new(),
    });

    let mut in_buffers = InBuffers::default();

    let mut tx_id: MessageId = 0;

    let (framed_out, mut framed_in) = Framed::new(io, SimpleChunkCodec).split();

    let cancel = CancellationToken::new();

    let tx_task_handle = tokio::spawn(tx_task::<Type, IO>(
        out_queues.clone(),
        framed_out,
        cancel.clone(),
    ));

    loop {
        select! {
            to_send = rx.recv() => {
                let Some(to_send) = to_send else {
                    tracing::info!("no more messages, closing task");
                    break;
                };

                let queue_id = to_send.queue_id();
                let encoded = OutMessage::encode(to_send, tx_id);

                // By the time we overflow, the first messages will
                // most certainly have been received.
                tx_id = tx_id.overflowing_add(1).0;

                out_queues.queues.lock().unwrap().entry(queue_id).or_default().messages.push_back(encoded);
                out_queues.can_send.notify_one();
            }
            received = framed_in.next() => {
                let Chunk { id, payload } = match received {
                    Some(Ok(c)) => c,
                    Some(Err(err)) => {
                        tracing::error!(?err, "failed to receive chunk");
                        break;
                    },
                    None => {
                        tracing::info!("stream closed, exiting task");
                        break;
                    },
                };

                let buffer = in_buffers
                    .partially_received
                    .entry(id)
                    .or_default();
                buffer.extend_from_slice(&payload);

                drop(payload);

                let decode_result = bincode::decode_from_slice(&buffer, bincode::config::standard());

                match decode_result {
                    Ok((message, size)) => {
                        let buffer = in_buffers.partially_received.remove(&id).unwrap();
                        assert_eq!(size, buffer.len());
                        drop(buffer);

                        if let Err(err) = tx.send(message).await {
                            tracing::info!(?err, "io task channel closed");
                            break;
                        }
                    },
                    Err(DecodeError::UnexpectedEnd { .. } ) => (),
                    Err(e) => {
                        tracing::error!("chunk decoding error: {e}");
                        break;
                    },
                }
            }
            _ = cancel.cancelled() => break
        }
    }

    cancel.cancel();

    let framed_out = tx_task_handle.await.expect("tx task paniced");
    let mut framed = framed_in.reunite(framed_out).unwrap();

    let _ = framed.close().await;
}

#[instrument(skip_all)]
async fn tx_task<Type: ProtocolEndpoint, IO: AsyncIO>(
    state: Arc<OutQueues<Type::OutMsg>>,
    mut framed: SplitSink<Framed<IO, SimpleChunkCodec>, Chunk>,
    cancel: CancellationToken,
) -> SplitSink<Framed<IO, SimpleChunkCodec>, Chunk> {
    let mut maybe_have_more = false;
    'outer: loop {
        select! {
            _ = state.can_send.notified(), if !maybe_have_more => (),
            _ = cancel.cancelled() => break,
        }

        let next_chunk = {
            let mut lock = state.queues.lock().unwrap();
            let mut rng = rand::rng();

            'find_queue: loop {
                let Some(id) = lock.keys().choose(&mut rng).cloned() else {
                    maybe_have_more = false;
                    continue 'outer;
                };
                let queue = lock.get_mut(&id).unwrap();

                break loop {
                    let Some(msg) = queue.messages.front_mut() else {
                        lock.remove(&id);
                        continue 'find_queue;
                    };

                    match msg.next() {
                        Some(chunk) => break chunk,
                        None => {
                            queue.messages.pop_front();
                        }
                    }
                }
            }
        };

        if let Err(err) = framed.send(next_chunk).await {
            tracing::error!(?err, "sending chunk failed");
            cancel.cancel();
            break 'outer;
        }
		maybe_have_more = true;
    }

    framed
}
