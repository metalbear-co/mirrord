use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    pin::Pin,
    sync::{Arc, atomic::AtomicUsize},
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Sink, SinkExt, Stream};
use mirrord_protocol::{
    DaemonMessage,
    outgoing::{
        DaemonConnect, DaemonConnectV2, DaemonRead, SocketAddress, UnixAddr,
        seqpacket::{DaemonSeqpacket, LayerSeqpacket},
    },
};
use tokio::io;
use tokio_seqpacket::UnixSeqpacket;

use crate::{
    metrics,
    outgoing::{
        GenericInMessage, GenericOutMessage, OutgoingKind,
        router::{ConnectionKind, NewConnection},
    },
    util::{io::throttle::Throttled, path_resolver::InTargetPathResolver},
};

pub struct SeqpacketConnection;

impl ConnectionKind for SeqpacketConnection {
    type Sink = SeqpacketSink;
    type Stream = SeqpacketStream;

    const DISPLAY_NAME: &'static str = "UNIX_SEQPACKET";

    async fn connect(
        addr: &SocketAddress,
        target_pid: Option<u64>,
    ) -> io::Result<NewConnection<Self>> {
        let path = match addr {
            SocketAddress::Unix(UnixAddr::Pathname(path)) => {
                if let Some(pid) = target_pid {
                    InTargetPathResolver::new(pid).resolve(path)?
                } else {
                    path.clone()
                }
            }
            SocketAddress::Unix(UnixAddr::Abstract(name)) => {
                let mut name = name.clone();
                name.insert(0, 0);
                OsString::from_vec(name).into()
            }
            SocketAddress::Unix(UnixAddr::Unnamed) => {
                return Err(io::Error::other("unexpected unnamed UNIX address"));
            }
            SocketAddress::Ip(addr) => {
                return Err(io::Error::other(format!("unexpected IP address: {addr}")));
            }
        };

        let socket = UnixSeqpacket::connect(path).await?;
        let socket = Arc::new(socket);
        let buffer = vec![0_u8; u16::MAX as usize].into_boxed_slice();

        Ok(NewConnection {
            sink: SeqpacketSink(Some(SeqpacketSinkState {
                socket: socket.clone(),
                ready_data: None,
            })),
            stream: SeqpacketStream(Some(SeqpacketStreamState { socket, buffer })),
            local_addr: SocketAddress::Unix(UnixAddr::Unnamed),
            peer_addr: addr.clone(),
        })
    }

    fn conn_counter() -> &'static AtomicUsize {
        &metrics::SEQPACKET_CONNECTION
    }
}

impl OutgoingKind for SeqpacketConnection {
    type InMessage = LayerSeqpacket;

    fn transform_in(message: Self::InMessage) -> GenericInMessage {
        match message {
            LayerSeqpacket::Write(layer_write) => {
                GenericInMessage::Write(layer_write.connection_id, layer_write.bytes.0)
            }
            LayerSeqpacket::Close(layer_close) => {
                GenericInMessage::Close(layer_close.connection_id)
            }
            LayerSeqpacket::ConnectV2(layer_connect_v2) => {
                GenericInMessage::Connect(layer_connect_v2.uid, layer_connect_v2.remote_address)
            }
        }
    }

    fn transform_out(message: GenericOutMessage) -> DaemonMessage {
        match message {
            GenericOutMessage::ConnectOk { uid: None, .. }
            | GenericOutMessage::ConnectErr { uid: None, .. } => unreachable!(),
            GenericOutMessage::ConnectOk {
                uid: Some(uid),
                id,
                local_addr,
                peer_addr,
            } => DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::ConnectV2(DaemonConnectV2 {
                uid,
                connect: Ok(DaemonConnect {
                    connection_id: id,
                    local_address: local_addr,
                    remote_address: peer_addr,
                }),
            })),
            GenericOutMessage::ConnectErr {
                uid: Some(uid),
                error,
            } => DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::ConnectV2(DaemonConnectV2 {
                uid,
                connect: Err(error.into()),
            })),
            GenericOutMessage::Read(id, bytes) => {
                DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Read(Ok(DaemonRead {
                    connection_id: id,
                    bytes: bytes.into(),
                })))
            }
            GenericOutMessage::Close(id) => {
                DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Close(id))
            }
        }
    }
}

pub struct SeqpacketStream(Option<SeqpacketStreamState>);

impl Stream for SeqpacketStream {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(state) = &mut this.0 else {
            return Poll::Ready(None);
        };
        let result = std::task::ready!(state.socket.poll_recv(cx, &mut state.buffer));
        match result {
            Ok(0) => {
                this.0 = None;
                Poll::Ready(None)
            }
            Ok(len) => Poll::Ready(Some(Ok(Bytes::copy_from_slice(
                state
                    .buffer
                    .get(..len)
                    .expect("poll_recv returned invalid length"),
            )))),
            Err(error) => {
                this.0 = None;
                Poll::Ready(Some(Err(error)))
            }
        }
    }
}

struct SeqpacketStreamState {
    socket: Arc<UnixSeqpacket>,
    buffer: Box<[u8]>,
}

pub struct SeqpacketSink(Option<SeqpacketSinkState>);

impl Sink<Throttled<Bytes>> for SeqpacketSink {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        if this.0.is_none() {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        };
        this.poll_flush_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Throttled<Bytes>) -> Result<(), Self::Error> {
        let this = self.get_mut();
        let Some(state) = &mut this.0 else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        if state.ready_data.is_none() {
            state.ready_data = Some(item);
            Ok(())
        } else {
            Err(io::Error::other("sink not ready"))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let Some(state) = &mut this.0 else {
            return Poll::Ready(Ok(()));
        };
        let Some(data) = &state.ready_data else {
            return Poll::Ready(Ok(()));
        };
        let result = match std::task::ready!(state.socket.poll_send(cx, data.as_ref())) {
            Ok(sent) if sent < data.len() => {
                this.0 = None;
                Err(io::Error::other(
                    "failed to send the whole message through the socket",
                ))
            }
            Ok(..) => {
                state.ready_data = None;
                Ok(())
            }
            Err(error) => {
                this.0 = None;
                Err(error)
            }
        };
        Poll::Ready(result)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_flush_unpin(cx))?;
        this.0 = None;
        Poll::Ready(Ok(()))
    }
}

struct SeqpacketSinkState {
    socket: Arc<UnixSeqpacket>,
    ready_data: Option<Throttled<Bytes>>,
}
