use std::{
    collections::HashMap,
    io::{self},
};

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use bincode::{Decode, Encode, error::DecodeError};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::{SinkExt, StreamExt};
use semver::Version;
use tokio::{select, sync::mpsc};
use tracing::instrument;

use crate::{
    ClientCodec, ClientMessage, DaemonCodec, DaemonMessage, Payload, ToPayload, VERSION,
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
const CHUNK_SIZE: u32 = 1024 * 64;

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

// struct OutQueue {
//     messages: VecDeque<OutMessage>,
// }

// #[derive(Default)]
// struct OutQueues<T> {
//     queues: HashMap<Discriminant<T>, OutQueue>,
// }

#[instrument(skip_all)]
async fn io_task_chunked<IO: AsyncIO, Type: ProtocolEndpoint>(
    io: IO,
    mut rx: mpsc::Receiver<Type::OutMsg>,
    tx: mpsc::Sender<Type::InMsg>,
) {
    // let out_queues = OutQueues::default();
    let mut in_buffers = InBuffers::default();

    // It would be nice if we could reuse these
    let mut tx_id: MessageId = 0;

    let mut framed = Framed::new(io, SimpleChunkCodec);

    loop {
        select! {
            to_send = rx.recv() => {
                let Some(to_send) = to_send else {
                    tracing::info!("no more messages, closing task");
                    break;
                };

                // Dumb
                for chunk in OutMessage::encode(&to_send, tx_id) {
                    if let Err(err) = framed.send(chunk).await {
                        tracing::error!(?err, "failed to send encoded chunk");
                        break;
                    }
                }

                // By the time we overflow, the first messages will
                // most certainly have been received.
                tx_id = tx_id.overflowing_add(1).0;

            }
            received = framed.next() => {
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


                let decode_result = bincode::decode_from_slice(&buffer, bincode::config::standard());

                match decode_result {
                    Ok((message, size)) => {
                        let buffer = in_buffers.partially_received.remove(&id).unwrap();
                        assert_eq!(size, buffer.len());
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
        }
    }

    let _ = framed.close().await;
}
