use core::fmt;
use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{Deref, DerefMut},
};

use futures::SinkExt;
use mirrord_protocol::{tcp::outgoing::*, ClientCodec, ClientMessage, ConnectionId};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::{error, info, trace, warn};

use crate::{
    common::{ResponseChannel, ResponseDeque},
    detour::DetourGuard,
    error::LayerError,
};

/// Wrapper type for the (layer) socket address that intercepts the user's socket messages.
#[derive(Debug)]
pub(crate) struct MirrorAddress(pub(crate) SocketAddr);

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SocketAddr,
    pub(crate) channel_tx: ResponseChannel<MirrorAddress>,
}

pub(crate) struct Write {
    pub(crate) connection_id: ConnectionId,
    pub(crate) bytes: Vec<u8>,
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Write")
            .field("id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

/// Hook messages handled by `TcpOutgoingHandler`.
#[derive(Debug)]
pub(crate) enum TcpOutgoing {
    Connect(Connect),
}

/// Responsible for handling hook and daemon messages for the outgoing traffic feature.
#[derive(Debug)]
pub(crate) struct TcpOutgoingHandler {
    /// Holds the channels used to send daemon messages to the interceptor socket, for the case
    /// where (agent) received data from the remote host, and sent it to (layer), to finally be
    /// passed all the way back to the user.
    mirrors: HashMap<ConnectionId, ConnectionMirror>,

    /// Holds the connection requests from the `connect` hook. It's main use is to reply back with
    /// the `SocketAddr` of the socket that'll be used to intercept the user's socket operations.
    connect_queue: ResponseDeque<MirrorAddress>,

    /// Channel used to pass messages (currently only `Write`) from an intercepted socket to the
    /// main `layer` loop.
    ///
    /// This is sent from `interceptor_task`.
    layer_tx: Sender<LayerTcpOutgoing>,
    layer_rx: Receiver<LayerTcpOutgoing>,
}

/// Wrapper type around `tokio::Sender`, used to send messages from the `agent` to our interceptor
/// socket, where they'll be written back to the user's socket.
///
/// (agent) -> (layer) -> (user)
#[derive(Debug)]
pub(crate) struct ConnectionMirror(tokio::sync::mpsc::Sender<Vec<u8>>);

impl Deref for ConnectionMirror {
    type Target = tokio::sync::mpsc::Sender<Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ConnectionMirror {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Default for TcpOutgoingHandler {
    fn default() -> Self {
        let (layer_tx, layer_rx) = channel(1000);

        Self {
            mirrors: Default::default(),
            connect_queue: Default::default(),
            layer_tx,
            layer_rx,
        }
    }
}

impl TcpOutgoingHandler {
    async fn interceptor_task(
        layer_tx: Sender<LayerTcpOutgoing>,
        connection_id: ConnectionId,
        mut mirror_stream: TcpStream,
        remote_rx: Receiver<Vec<u8>>,
    ) {
        let mut remote_stream = ReceiverStream::new(remote_rx);
        let mut buffer = vec![0; 1024];

        // Sends a message to close the remote stream in `agent`, when it's
        // being closed in `layer`.
        //
        // This happens when the `mirror_stream` has no more data to receive, or when it fails
        // `read`ing.
        let close_remote_stream = |layer_tx: Sender<_>| async move {
            let close = LayerClose { connection_id };
            let outgoing_close = LayerTcpOutgoing::Close(close);

            if let Err(fail) = layer_tx.send(outgoing_close).await {
                error!("Failed sending close message with {:#?}!", fail);
            }
        };

        loop {
            select! {
                biased; // To allow local socket to be read before being closed

                // Reads data that the user is sending from their socket to mirrord's interceptor
                // socket.
                read = mirror_stream.read(&mut buffer) => {
                    match read {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => {
                            error!("Failed reading mirror_stream with {:#?}", fail);
                            close_remote_stream(layer_tx.clone()).await;

                            break;
                        }
                        Ok(read_amount) if read_amount == 0 => {
                            info!("interceptor_task -> Stream {:#?} has no more data, closing!", connection_id);
                            close_remote_stream(layer_tx.clone()).await;

                            break;
                        },
                        Ok(read_amount) => {
                            // Sends the message that the user wrote to our interceptor socket to
                            // be handled on the `agent`, where it'll be forwarded to the remote.
                            let write = LayerWrite { connection_id, bytes: buffer[..read_amount].to_vec() };
                            let outgoing_write = LayerTcpOutgoing::Write(write);

                            if let Err(fail) = layer_tx.send(outgoing_write).await {
                                error!("Failed sending write message with {:#?}!", fail);

                                break;
                            }
                        }
                    }
                },
                bytes = remote_stream.next() => {
                    match bytes {
                        Some(bytes) => {
                            // Writes the data sent by `agent` (that came from the actual remote
                            // stream) to our interceptor socket. When the user tries to read the
                            // remote data, this'll be what they receive.
                            if let Err(fail) = mirror_stream.write_all(&bytes).await {
                                error!("Failed writing to mirror_stream with {:#?}!", fail);
                                break;
                            }
                        },
                        None => {
                            warn!("interceptor_task -> exiting due to remote stream closed!");
                            break;
                        }
                    }
                },
            }
        }

        trace!(
            "interceptor_task done -> connection_id {:#?}",
            connection_id
        );
    }

    /// Handles the following hook messages:
    ///
    /// - `TcpOutgoing::Connect`: inserts the new connection request into a connection queue, and
    ///   sends it to (agent) as a `TcpOutgoingRequest::Connect` with the remote host's address.
    ///
    /// - `TcpOutgoing::Write`: sends a `TcpOutgoingRequest::Write` message to (agent) with the data
    ///   that our interceptor socket intercepted.
    pub(crate) async fn handle_hook_message(
        &mut self,
        message: TcpOutgoing,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        trace!("handle_hook_message -> message {:?}", message);

        match message {
            TcpOutgoing::Connect(Connect {
                remote_address,
                channel_tx,
            }) => {
                trace!("Connect -> remote_address {:#?}", remote_address);

                // TODO: We could be losing track of the proper order to respond to these (aviram
                // suggests using a `HashMap`).
                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(
                        LayerConnect { remote_address },
                    )))
                    .await?)
            }
        }
    }

    /// Handles the following daemon messages:
    ///
    /// - `TcpOutgoingResponse::Connect`: grabs the reply from the connection request that was sent
    ///   to (agent), then creates a new `TcpListener` (the interceptor socket) that the user socket
    ///   will connect to. When everything succeeds, it spawns a new task that handles the
    ///   communication between user and interceptor sockets.
    ///
    /// - `TcpOutgoingResponse::Read`: (agent) received some data from the remote host and sent it
    ///   back to (layer). The data will be sent to our interceptor socket, which in turn will send
    ///   it back to the user socket.
    ///
    /// - `TcpOutgoingResponse::Write`: (agent) sent some data to the remote host, currently this
    ///   response is only significant to handle errors when this send failed.
    pub(crate) async fn handle_daemon_message(
        &mut self,
        response: DaemonTcpOutgoing,
    ) -> Result<(), LayerError> {
        trace!("handle_daemon_message -> message {:?}", response);

        match response {
            DaemonTcpOutgoing::Connect(connect) => {
                trace!("Connect -> connect {:#?}", connect);

                let DaemonConnect {
                    connection_id,
                    remote_address,
                } = connect?;

                let mirror_stream = {
                    let _ = DetourGuard::new();

                    let mirror_listener = match remote_address {
                        SocketAddr::V4(_) => {
                            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                                .await?
                        }
                        SocketAddr::V6(_) => {
                            TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
                                .await?
                        }
                    };

                    // Creates the listener that will wait for the user's socket connection.
                    let mirror_address = MirrorAddress(mirror_listener.local_addr()?);

                    self.connect_queue
                        .pop_front()
                        .ok_or(LayerError::SendErrorTcpResponse)?
                        .send(Ok(mirror_address))
                        .map_err(|_| LayerError::SendErrorTcpResponse)?;

                    // Accepts the user's socket connection, and finally becomes the interceptor
                    // socket.
                    let (mirror_stream, _) = mirror_listener.accept().await?;
                    mirror_stream
                };

                let (remote_tx, remote_rx) = channel::<Vec<u8>>(1000);

                self.mirrors
                    .insert(connection_id, ConnectionMirror(remote_tx));

                // user and interceptor sockets are connected to each other, so now we spawn a new
                // task to pair their reads/writes.
                task::spawn(TcpOutgoingHandler::interceptor_task(
                    self.layer_tx.clone(),
                    connection_id,
                    mirror_stream,
                    remote_rx,
                ));

                Ok(())
            }
            DaemonTcpOutgoing::Read(read) => {
                // (agent) read something from remote, so we write it to the user.
                trace!("Read -> read {:?}", read);
                let DaemonRead {
                    connection_id,
                    bytes,
                } = read?;

                let sender = self
                    .mirrors
                    .get_mut(&connection_id)
                    .ok_or(LayerError::NoConnectionId(connection_id))?;

                Ok(sender.send(bytes).await?)
            }
            DaemonTcpOutgoing::Close(connection_id) => {
                // (agent) failed to perform some operation.
                trace!("Close -> connection_id {:?}", connection_id);
                self.mirrors.remove(&connection_id);

                Ok(())
            }
        }
    }

    /// Helper function to access the channel of messages that are to be passed directly as
    /// `ClientMessage` from `layer`.
    pub(crate) fn recv(&mut self) -> impl Future<Output = Option<LayerTcpOutgoing>> + '_ {
        self.layer_rx.recv()
    }
}
