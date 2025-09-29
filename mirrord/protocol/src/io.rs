use std::io;

use actix_codec::{AsyncRead, AsyncWrite, Decoder, Encoder, Framed};
use futures::{FutureExt, SinkExt, StreamExt};
use tokio::{select, sync::mpsc};

use crate::{
    ClientCodec, ClientMessage, DaemonCodec, DaemonMessage, VERSION, file::CHUNKED_PROTOCOL_VERSION,
};

pub trait AsyncIO: AsyncWrite + AsyncRead + Send + Unpin {}
impl<T: AsyncWrite + AsyncRead + Send + Unpin> AsyncIO for T {}

pub enum Mode {
    Legacy,
    Chunked,
}

pub trait ProtocolEndpoint: 'static + Sized {
    type InMsg: bincode::Decode<()> + Send;
    type OutMsg: bincode::Encode + Send;
    type Codec: Encoder<Self::OutMsg, Error = io::Error>
        + Decoder<Item = Self::InMsg, Error = io::Error>
        + Default
        + Send;

    async fn negotiate<IO: AsyncIO>(io: &mut Framed<IO, Self::Codec>) -> Result<Mode, ProtocolError>;
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

    async fn negotiate<IO: AsyncIO>(io: &mut Framed<IO, Self::Codec>) -> Result<Mode, ProtocolError> {
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

        if CHUNKED_PROTOCOL_VERSION.matches(&version) {
            Ok(Mode::Chunked)
        } else {
            Ok(Mode::Legacy)
        }
    }
}

impl ProtocolEndpoint for Agent {
    type InMsg = ClientMessage;
    type OutMsg = DaemonMessage;
    type Codec = DaemonCodec;

    async fn negotiate<IO: AsyncIO>(io: &mut Framed<IO, Self::Codec>) -> Result<Mode, ProtocolError> {
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

        let version = Ord::min(&client_version, &VERSION);

        io.send(DaemonMessage::SwitchProtocolVersionResponse(
            version.clone(),
        ))
        .await?;

        if CHUNKED_PROTOCOL_VERSION.matches(&version) {
            Ok(Mode::Chunked)
        } else {
            Ok(Mode::Legacy)
        }
    }
}

pub struct Connection<Type: ProtocolEndpoint> {
    pub tx: mpsc::Sender<Type::OutMsg>,
    pub rx: mpsc::Receiver<Type::InMsg>,
}

impl<Type: ProtocolEndpoint> Connection<Type> {
    pub async fn new<IO: AsyncIO + 'static>(inner: IO) -> Result<Self, ProtocolError> {
        let mut framed = Framed::new(inner, Type::Codec::default());
        let mode = Type::negotiate(&mut framed).await?;
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

async fn io_task_chunked<IO: AsyncIO, Type: ProtocolEndpoint>(
    _io: IO,
    _rx: mpsc::Receiver<Type::OutMsg>,
    _tx: mpsc::Sender<Type::InMsg>,
) {
    unimplemented!()
}
