use std::{
    collections::{HashMap, VecDeque},
    io,
    mem::Discriminant,
};

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use bincode::{error::DecodeError, Decode, Encode};
use bytes::Bytes;
use futures::{FutureExt, SinkExt, StreamExt};
use semver::Version;
use tokio::{select, sync::mpsc};

use crate::{
    ClientCodec, ClientMessage, DaemonCodec, DaemonMessage, Payload, ProtocolCodec, VERSION,
    file::CHUNKED_PROTOCOL_VERSION,
};

pub trait AsyncIO: AsyncWrite + AsyncRead + Send + Unpin {}
impl<T: AsyncWrite + AsyncRead + Send + Unpin> AsyncIO for T {}

pub enum Mode {
    Legacy,
    Chunked,
}

pub trait ProtocolEndpoint: 'static + Sized {
    type InMsg: bincode::Decode<()> + Send + std::fmt::Debug;
    type OutMsg: bincode::Encode + Send + std::fmt::Debug;
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
    pub async fn new<IO: AsyncIO + 'static>(inner: IO) -> Result<Self, ProtocolError> {
        let mut framed = Framed::new(inner, Type::Codec::default());
        let (mode, version) = Type::negotiate(&mut framed).await?;
        let (inbound_tx, inbound_rx) = mpsc::channel(1024);
        let (outbound_tx, outbound_rx) = mpsc::channel(1024);

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

async fn io_task_legacy<IO: AsyncIO, Type: ProtocolEndpoint>(
    mut framed: Framed<IO, Type::Codec>,
    mut rx: mpsc::Receiver<Type::OutMsg>,
    tx: mpsc::Sender<Type::InMsg>,
) {
    loop {
        select! {
            // REVIEW: handle errors gracefully
            to_send = rx.recv().fuse() => {
                let to_send = to_send.expect("io task channel closed");
                framed.send(to_send).await.expect("failed to send");
            }
            // REVIEW: Cancel safety?
            received = framed.next().fuse() => {
                match received {
                    None => panic!("failed to receive"),
                    Some(Ok(e)) => tx.send(e).await.expect("io task channel closed"),
                    Some(Err(err)) => panic!("invalid message received: {err:?}")
                }
            }
        }
    }
}

#[derive(Debug, Encode, Decode)]
struct Chunk {
    id: u32,
    payload: Payload,
}

struct OutMessage {
    encoded: Bytes,
    next_chunk_offset: u32,
    id: u32,
}

impl OutMessage {
    fn encode<T: bincode::Encode>(message: T, id: u32) -> Self {
        let encoded = bincode::encode_to_vec(message, bincode::config::standard())
            .expect("failed to encode message")
            .into();
        Self {
            encoded,
            next_chunk_offset: 0,
            id,
        }
    }
}

const CHUNK_SIZE: u32 = 1024 * 8;

impl Iterator for OutMessage {
    type Item = Chunk;

    fn next(&mut self) -> Option<Self::Item> {
        let size = Ord::min(
            CHUNK_SIZE,
            self.encoded.len() as u32 - self.next_chunk_offset,
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

struct OutQueue {
    messages: VecDeque<OutMessage>,
}

#[derive(Default)]
struct OutQueues<T> {
    queues: HashMap<Discriminant<T>, OutQueue>,
}

#[derive(Default)]
struct InBuffers {
    partially_received: HashMap<u32, Vec<u8>>,
}

async fn io_task_chunked<IO: AsyncIO, Type: ProtocolEndpoint>(
    io: IO,
    mut rx: mpsc::Receiver<Type::OutMsg>,
    tx: mpsc::Sender<Type::InMsg>,
) {
    // REVIEW: handle errors gracefully
    // let out_queues = OutQueues::default();
    let mut in_buffers = InBuffers::default();

    let mut chunk_codec = Framed::new(io, ProtocolCodec::<Chunk, Chunk>::default());

    let mut tx_id: u32 = 0;

    loop {
        select! {
            to_send = rx.recv().fuse() => {
                let to_send = to_send.expect("io task channel closed");
                // Dumb
                for chunk in OutMessage::encode(to_send, tx_id) {
                    chunk_codec.send(chunk).await.expect("failed 2 send");
                }

                tx_id += 1;

            }
            // REVIEW: Cancel safety?
            received = chunk_codec.next().fuse() => {
                let Chunk { id, payload } = match received {
                    None => panic!("failed to receive"),
                    Some(Err(err)) => panic!("invalid message received: {err:?}"),
                    Some(Ok(c)) => c,
                };

                let buffer = in_buffers
                    .partially_received
                    .entry(id)
                .or_default();
                buffer.extend_from_slice(&payload);

                match bincode::decode_from_slice(&buffer, bincode::config::standard()){
                    Ok((message, size)) => {
                        let buffer = in_buffers.partially_received.remove(&id).unwrap();
                        assert_eq!(size, buffer.len());
                        tx.send(message).await.expect("io task channel closed");
                    },
                    Err(DecodeError::UnexpectedEnd { .. } ) => (),
					Err(e) => panic!("chunk decoding error: {e}"),
                }
            }
        }
    }
}
