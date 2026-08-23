use std::{
    mem::MaybeUninit,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::{Arc, atomic::AtomicUsize},
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use bytes::Bytes;
use futures::{Sink, SinkExt, Stream};
use mirrord_protocol::{
    DaemonMessage,
    outgoing::{
        DaemonConnect, DaemonConnectV2, DaemonRead, SocketAddress,
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
};
use tokio::{io, net::UdpSocket};

use crate::{
    metrics,
    outgoing::{
        GenericInMessage, GenericOutMessage, OutgoingKind,
        router::{ConnectionKind, NewConnection},
    },
    util::io::throttle::Throttled,
};

pub struct UdpConnection;

impl ConnectionKind for UdpConnection {
    type Sink = UdpSink;
    type Stream = UdpStream;

    const DISPLAY_NAME: &'static str = "UDP";

    async fn connect(addr: &SocketAddress, _: Option<u64>) -> io::Result<NewConnection<Self>> {
        let addr = match addr {
            SocketAddress::Ip(addr) => *addr,
            SocketAddress::Unix(..) => {
                return Err(io::Error::other(format!("unexpected UNIX address: {addr}")));
            }
        };

        let bind_addr = match addr.ip() {
            IpAddr::V4(..) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(..) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let bind_addr = SocketAddr::new(bind_addr, 0);
        let socket = UdpSocket::bind(bind_addr).await?;
        socket.connect(addr).await?;
        let local_addr = socket.local_addr()?;
        let peer_addr = socket.peer_addr()?;
        let socket = Arc::new(socket);

        Ok(NewConnection {
            sink: UdpSink(Some(UdpSinkState {
                socket: socket.clone(),
                ready_data: None,
            })),
            stream: UdpStream(Some(UdpStreamState {
                socket,
                buffer: Box::new_uninit_slice(u16::MAX as usize),
            })),
            local_addr: local_addr.into(),
            peer_addr: peer_addr.into(),
        })
    }

    fn conn_counter() -> &'static AtomicUsize {
        &metrics::UDP_OUTGOING_CONNECTION
    }
}

impl OutgoingKind for UdpConnection {
    type InMessage = LayerUdpOutgoing;

    fn transform_in(message: Self::InMessage) -> GenericInMessage {
        match message {
            LayerUdpOutgoing::Connect(layer_connect) => {
                GenericInMessage::ConnectLegacy(layer_connect.remote_address)
            }
            LayerUdpOutgoing::Write(layer_write) => {
                GenericInMessage::Write(layer_write.connection_id, layer_write.bytes.0)
            }
            LayerUdpOutgoing::Close(layer_close) => {
                GenericInMessage::Close(layer_close.connection_id)
            }
            LayerUdpOutgoing::ConnectV2(layer_connect_v2) => {
                GenericInMessage::Connect(layer_connect_v2.uid, layer_connect_v2.remote_address)
            }
        }
    }

    fn transform_out(message: GenericOutMessage) -> DaemonMessage {
        match message {
            GenericOutMessage::ConnectOk {
                uid: None,
                id,
                local_addr,
                peer_addr,
            } => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Ok(DaemonConnect {
                connection_id: id,
                remote_address: peer_addr,
                local_address: local_addr,
            }))),
            GenericOutMessage::ConnectOk {
                uid: Some(uid),
                id,
                local_addr,
                peer_addr,
            } => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::ConnectV2(DaemonConnectV2 {
                uid,
                connect: Ok(DaemonConnect {
                    connection_id: id,
                    remote_address: peer_addr,
                    local_address: local_addr,
                }),
            })),
            GenericOutMessage::ConnectErr { uid: None, error } => {
                DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Err(error.into())))
            }
            GenericOutMessage::ConnectErr {
                uid: Some(uid),
                error,
            } => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::ConnectV2(DaemonConnectV2 {
                uid,
                connect: Err(error.into()),
            })),
            GenericOutMessage::Read(id, bytes) => {
                DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(DaemonRead {
                    connection_id: id,
                    bytes: bytes.into(),
                })))
            }
            GenericOutMessage::Close(id) => {
                DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(id))
            }
        }
    }
}

pub struct UdpSink(Option<UdpSinkState>);

impl Sink<Throttled<Bytes>> for UdpSink {
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
                    "failed to send the whole datagram through the socket",
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

struct UdpSinkState {
    socket: Arc<UdpSocket>,
    ready_data: Option<Throttled<Bytes>>,
}

pub struct UdpStream(Option<UdpStreamState>);

impl Stream for UdpStream {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(state) = &mut this.0 else {
            return Poll::Ready(None);
        };
        let mut read_buf = ReadBuf::uninit(&mut state.buffer);
        match std::task::ready!(state.socket.poll_recv(cx, &mut read_buf)) {
            Ok(()) if read_buf.filled().is_empty() => {
                this.0 = None;
                Poll::Ready(None)
            }
            Ok(()) => Poll::Ready(Some(Ok(Bytes::copy_from_slice(read_buf.filled())))),
            Err(error) => {
                this.0 = None;
                Poll::Ready(Some(Err(error)))
            }
        }
    }
}

struct UdpStreamState {
    socket: Arc<UdpSocket>,
    buffer: Box<[MaybeUninit<u8>]>,
}
