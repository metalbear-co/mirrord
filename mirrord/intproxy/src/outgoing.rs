use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    marker::PhantomData,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{Deref, DerefMut},
};

use futures::TryFutureExt;
use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        LayerConnect, SocketAddress, tcp::{LayerTcpOutgoing, DaemonTcpOutgoing}, DaemonConnect, DaemonRead,
    },
    ClientMessage, ConnectionId,
    ResponseError::NotImplemented, RemoteResult,
};
use tokio::{
    net::UdpSocket,
    select,
    sync::mpsc::{self, Receiver, Sender},
    task,
};

use crate::{
    agent_conn::AgentSender,
    error::{Result, IntProxyError},
    layer_conn::LayerSender,
    protocol::{ConnectTcpOutgoing, MessageId, ConnectUdpOutgoing, LocalMessage, ProxyToLayerMessage, AgentResponse},
    request_queue::RequestQueue,
};

pub enum AgentOutgoingMessage {
    Connect(RemoteResult<DaemonConnect>),
    Read(RemoteResult<DaemonRead>),
    Close(ConnectionId),
}

pub trait Protocol {
    type AgentMessage;

    fn make_connect_message(remote_address: SocketAddress) -> ClientMessage;

    fn make_connect_response(result: RemoteResult<DaemonConnect>) -> AgentResponse;

    fn get_agent_message(message: Self::AgentMessage) -> AgentOutgoingMessage;
}

pub struct Tcp;

impl Protocol for Tcp {
    type AgentMessage = DaemonTcpOutgoing;

    fn make_connect_message(remote_address: SocketAddress) -> ClientMessage {
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address }))
    }

    fn get_agent_message(message: Self::AgentMessage) -> AgentOutgoingMessage {
        match message {
            DaemonTcpOutgoing::Close(close) => AgentOutgoingMessage::Close(close),
            DaemonTcpOutgoing::Connect(connect) => AgentOutgoingMessage::Connect(connect),
            DaemonTcpOutgoing::Read(read) => AgentOutgoingMessage::Read(read),
        }
    }
}

pub struct Udp;

impl Protocol for Udp {
    type AgentMessage = DaemonUdpOutgoing;

    fn make_connect_message(remote_address: SocketAddress) -> ClientMessage {
        ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address }))
    }

    fn get_agent_message(message: Self::AgentMessage) -> AgentOutgoingMessage {
        match message {
            DaemonUdpOutgoing::Close(close) => AgentOutgoingMessage::Close(close),
            DaemonUdpOutgoing::Connect(connect) => AgentOutgoingMessage::Connect(connect),
            DaemonUdpOutgoing::Read(read) => AgentOutgoingMessage::Read(read),
        }
    }
}

#[derive(Debug)]
struct LayerSocketConnection {}

#[derive(Debug, Clone)]
struct LayerSocketSender(Sender<Vec<u8>>);

pub struct OutgoingHandler<P> {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    layer_sockets: HashMap<ConnectionId, LayerSocketSender>,
    connect_queue: RequestQueue,
    _phantom: PhantomData<fn() -> P>,
}

impl<P> OutgoingHandler<P> {
    pub fn new(agent_sender: AgentSender, layer_sender: LayerSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            layer_sockets: Default::default(),
            connect_queue: Default::default(),
            _phantom: Default::default(),
        }
    }
}

impl<P: Protocol> OutgoingHandler<P> {
    pub async fn handle_connect_request(
        &mut self,
        request_id: MessageId,
        remote_address: SocketAddress,
    ) -> Result<()> {
        self.connect_queue.save_request_id(request_id);

        self.agent_sender
            .send(P::make_connect_message(remote_address))
            .await?;

        Ok(())
    }

    pub async fn handle_agent_message(&mut self, message: P::AgentMessage) -> Result<()> {
        let message = P::get_agent_message(message);

        match message {
            AgentOutgoingMessage::Close(inner) => self.handle_agent_close(inner).await,
            AgentOutgoingMessage::Read(inner) => self.handle_agent_read(inner).await,
            AgentOutgoingMessage::Connect(inner) => self.handle_agent_connect(inner).await,
        }
    }

    async fn handle_agent_connect(&mut self, result: RemoteResult<DaemonConnect>) -> Result<()> {
        match result {
            Ok(daemon_connect) => {
                todo!()
            },
            Err(err) => {
                let message_id = self.connect_queue.get_request_id().ok_or(IntProxyError::RequestQueueEmpty)?;
                self.layer_sender.send(LocalMessage {
                    message_id,
                    inner: ProxyToLayerMessage::AgentResponse(P::make_connect_response(Err(err))),
                }).await?;
            }
        }

        Ok(())
    }

    async fn handle_agent_close(&mut self, connection_id: ConnectionId) -> Result<()> {
        todo!()
    }

    async fn handle_agent_read(&mut self, result: RemoteResult<DaemonRead>) -> Result<()> {
        todo!()
    } 
}

// pub async fn handle_
// async fn interceptor_task(
//     layer_tx: Sender<LayerUdpOutgoing>,
//     connection_id: ConnectionId,
//     mirror_socket: UdpSocket,
//     remote_rx: Receiver<Vec<u8>>,
// ) { let mut remote_stream = ReceiverStream::new(remote_rx); let mut recv_from_buffer = vec![0;
//   1500];

//     // Sends a message to close the remote stream in `agent`, when it's
//     // being closed in `layer`.
//     //
//     // This happens when the `mirror_stream` has no more data to receive, or when it fails
//     // `read`ing.
//     let close_remote_stream = |layer_tx: Sender<_>| async move {
//         let close = LayerClose { connection_id };
//         let outgoing_close = LayerUdpOutgoing::Close(close);

//         if let Err(fail) = layer_tx.send(outgoing_close).await {
//             error!("Failed sending close message with {:#?}!", fail);
//         }
//     };

//     let mut user_address: Option<SocketAddr> = None;

//     loop {
//         select! {
//             biased; // To allow local socket to be read before being closed

//             read = mirror_socket.recv_from(&mut recv_from_buffer) => {
//                 match read {
//                     Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
//                         continue;
//                     },
//                     Err(fail) => {
//                         info!("Failed reading mirror_stream with {:#?}", fail);
//                         close_remote_stream(layer_tx.clone()).await;

//                         break;
//                     }
//                     Ok((0, _)) => {
//                         trace!("interceptor_task -> Stream {:#?} has no more data, closing!",
// connection_id);                         close_remote_stream(layer_tx.clone()).await;

//                         break;
//                     },
//                     Ok((read_amount, from)) => {
//                         trace!("interceptor_task -> Received data from user socket {:#?}", from);
//                         user_address = Some(from);
//                         // Sends the message that the user wrote to our interceptor socket to
//                         // be handled on the `agent`, where it'll be forwarded to the remote.
//                         let write = LayerWrite {
//                             connection_id,
//                             bytes: recv_from_buffer
//                                 .get(..read_amount)
//                                 .expect("recv_from returned more bytes than the buffer can hold")
//                                 .to_vec(),
//                         };
//                         let outgoing_write = LayerUdpOutgoing::Write(write);

//                         if let Err(fail) = layer_tx.send(outgoing_write).await {
//                             error!("Failed sending write message with {:#?}!", fail);

//                             break;
//                         }
//                     }
//                 }
//             },
//             bytes = remote_stream.next() => {
//                 match bytes {
//                     Some(bytes) => {
//                         trace!("interceptor_task -> Received data from remote socket");
//                         // Writes the data sent by `agent` (that came from the actual remote
//                         // stream) to our interceptor socket. When the user tries to read the
//                         // remote data, this'll be what they receive.
//                         if let Err(fail) = mirror_socket
//                             .send_to(
//                                 &bytes,
//                                 user_address.expect("User socket should be set by now!"),
//                             )
//                             .await
//                         {
//                             trace!("Failed writing to mirror_stream with {:#?}!", fail);
//                             break;
//                         }
//                     },
//                     None => {
//                         warn!("interceptor_task -> exiting due to remote stream closed!");
//                         break;
//                     }
//                 }
//             },
//         }
//     }

//     trace!(
//         "interceptor_task done -> connection_id {:#?}",
//         connection_id
//     );
// }

// /// Handles the following hook messages:
// ///
// /// - `UdpOutgoing::Connect`: inserts the new connection request into a connection queue, and
// ///   sends it to (agent) as a `UdpOutgoingRequest::Connect` with the remote host's address.
// ///
// /// - `UdpOutgoing::Write`: sends a `UdpOutgoingRequest::Write` message to (agent) with the data
// ///   that our interceptor socket intercepted.
// #[tracing::instrument(level = "trace", ret, skip(self, tx))]
// pub(crate) async fn handle_hook_message(
//     &mut self,
//     message: UdpOutgoing,
//     tx: &Sender<ClientMessage>,
// ) -> Result<(), LayerError> { match message { UdpOutgoing::Connect(Connect { remote_address,
//   channel_tx, }) => { // TODO(alex): We need to check if this `remote_address` is actually a
//   local // address! If it is, then the `agent` won't be able to reach it. // Right now we're
//   sidestepping this issue by just changing the address in `agent` // to be a "local-agent"
//   address. // // Have to be careful, as this is rather finnicky behavior when the user wants to
//   // make a request to their own local address. trace!("Connect -> remote_address {:#?}",
//   remote_address);

//             // TODO: We could be losing track of the proper order to respond to these (aviram
//             // suggests using a `HashMap`).
//             self.connect_queue.push_back(channel_tx);

//             Ok(tx
//                 .send(ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(
//                     LayerConnect {
//                         remote_address: remote_address.try_into()?,
//                     },
//                 )))
//                 .await?)
//         }
//     }
// }

// /// Handles the following daemon messages:
// ///
// /// - `UdpOutgoingResponse::Connect`: grabs the reply from the connection request that was sent
// ///   to (agent), then creates a new `UdpListener` (the interceptor socket) that the user socket
// ///   will connect to. When everything succeeds, it spawns a new task that handles the
// ///   communication between user and interceptor sockets.
// ///
// /// - `UdpOutgoingResponse::Read`: (agent) received some data from the remote host and sent it
// ///   back to (layer). The data will be sent to our interceptor socket, which in turn will send
// ///   it back to the user socket.
// ///
// /// - `UdpOutgoingResponse::Write`: (agent) sent some data to the remote host, currently this
// ///   response is only significant to handle errors when this send failed.
// #[tracing::instrument(level = "trace", ret, skip(self))]
// pub(crate) async fn handle_daemon_message(
//     &mut self,
//     response: DaemonUdpOutgoing,
// ) -> Result<(), LayerError> { match response { DaemonUdpOutgoing::Connect(connect) => { let
//   response = async move { connect } .and_then( |DaemonConnect { connection_id, remote_address,
//   local_address, }| async move { let _ = DetourGuard::new();

//                         let mirror_socket = match remote_address {
//                             SocketAddress::Ip(SocketAddr::V4(_)) => UdpSocket::bind(
//                                 SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
//                             ),
//                             SocketAddress::Ip(SocketAddr::V6(_)) => UdpSocket::bind(
//                                 SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
//                             ),
//                             SocketAddress::Unix(_) => {
//                                 // This should never happen. If we're here - the agent reported
//                                 // a UDP connection to a remote unix socket. The layer does not
//                                 // request such a thing, so either there was a bug earlier in
//                                 // the layer, the agent is rogue, or this code is outdated.
//                                 error!("Datagrams over unix sockets are not supported.");
//                                 Err(NotImplemented)?
//                             }
//                         }
//                         .await?;

//                         Ok((connection_id, mirror_socket, local_address))
//                     },
//                 )
//                 .await
//                 .and_then(|(connection_id, socket, user_app_address)| {
//                     let (remote_tx, remote_rx) = channel::<Vec<u8>>(1000);

//                     let _ = DetourGuard::new();
//                     let layer_address = socket.local_addr()?;

//                     // user and interceptor sockets are connected to each other, so now we spawn
//                     // a new task to pair their reads/writes.
//                     task::spawn(UdpOutgoingHandler::interceptor_task(
//                         self.layer_tx.clone(),
//                         connection_id,
//                         socket,
//                         remote_rx,
//                     ));

//                     self.mirrors
//                         .insert(connection_id, ConnectionMirror(remote_tx));

//                     Ok(RemoteConnection {
//                         user_app_address: user_app_address.try_into()?,
//                         layer_address: layer_address.into(),
//                     })
//                 });

//             self.connect_queue
//                 .pop_front()
//                 .ok_or(LayerError::SendErrorUdpResponse)?
//                 .send(response)
//                 .map_err(|_| LayerError::SendErrorUdpResponse)
//         }
//         DaemonUdpOutgoing::Read(read) => {
//             // (agent) read something from remote, so we write it to the user.
//             let DaemonRead {
//                 connection_id,
//                 bytes,
//             } = read?;

//             let sender = self
//                 .mirrors
//                 .get_mut(&connection_id)
//                 .ok_or(LayerError::NoConnectionId(connection_id))?;

//             Ok(sender.send(bytes).await?)
//         }
//         DaemonUdpOutgoing::Close(connection_id) => {
//             // (agent) failed to perform some operation.
//             self.mirrors.remove(&connection_id);

//             Ok(())
//         }
//     }
// }

// /// Helper function to access the channel of messages that are to be passed directly as
// /// `ClientMessage` from `layer`.
// pub(crate) fn recv(&mut self) -> impl Future<Output = Option<LayerUdpOutgoing>> + '_ {
//     self.layer_rx.recv()
// }
