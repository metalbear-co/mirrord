use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use futures::TryFutureExt;
use mirrord_protocol::{
    outgoing::{tcp::*, DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite},
    ClientMessage, ConnectionId,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::{error, info, trace, warn};

use super::*;
use crate::{common::ResponseDeque, detour::DetourGuard, error::LayerError};

/// Hook messages handled by `TcpOutgoingHandler`.
///
/// - Part of [`HookMessage`](crate::common::HookMessage).
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
    connect_queue: ResponseDeque<RemoteConnection>,

    /// Channel used to pass messages (currently only `Write`) from an intercepted socket to the
    /// main `layer` loop.
    ///
    /// This is sent from `interceptor_task`.
    layer_tx: Sender<LayerTcpOutgoing>,
    layer_rx: Receiver<LayerTcpOutgoing>,
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
    #[tracing::instrument(level = "trace", skip(layer_tx, mirror_listener, remote_rx))]
    async fn interceptor_task(
        layer_tx: Sender<LayerTcpOutgoing>,
        connection_id: ConnectionId,
        mirror_listener: TcpListener,
        remote_rx: Receiver<Vec<u8>>,
    ) {
        // Accepts the user's socket connection, and finally becomes the interceptor socket.
        let (mut mirror_stream, _) = mirror_listener.accept().await.unwrap();

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
                            info!("Failed reading mirror_stream with {:#?}", fail);
                            close_remote_stream(layer_tx.clone()).await;

                            break;
                        }
                        Ok(read_amount) if read_amount == 0 => {
                            trace!("interceptor_task -> Stream {:#?} has no more data, closing!", connection_id);
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
                                trace!("Failed writing to mirror_stream with {:#?}!", fail);
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
    }

    /// Handles the following hook messages:
    ///
    /// - `TcpOutgoing::Connect`: inserts the new connection request into a connection queue, and
    ///   sends it to (agent) as a `TcpOutgoingRequest::Connect` with the remote host's address.
    ///
    /// - `TcpOutgoing::Write`: sends a `TcpOutgoingRequest::Write` message to (agent) with the data
    ///   that our interceptor socket intercepted.
    #[tracing::instrument(level = "trace", skip(self, tx))]
    pub(crate) async fn handle_hook_message(
        &mut self,
        message: TcpOutgoing,
        tx: &Sender<ClientMessage>,
    ) -> Result<(), LayerError> {
        match message {
            TcpOutgoing::Connect(Connect {
                remote_address,
                channel_tx,
            }) => {
                trace!("Connect -> remote_address {:#?}", remote_address);

                // TODO: We could be losing track of the proper order to respond to these (aviram
                // suggests using a `HashMap`).
                self.connect_queue.push_back(channel_tx);

                Ok(tx
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
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn handle_daemon_message(
        &mut self,
        response: DaemonTcpOutgoing,
    ) -> Result<(), LayerError> {
        match response {
            DaemonTcpOutgoing::Connect(connect) => {
                trace!("Connect -> connect {:#?}", connect);

                let response = async move { connect }
                    .and_then(
                        |DaemonConnect {
                             connection_id,
                             remote_address,
                             local_address,
                         }| async move {
                            let _ = DetourGuard::new();

                            // Creates the listener that will wait for the user's socket
                            // connection.
                            let mirror_listener = match remote_address {
                                SocketAddr::V4(_) => {
                                    TcpListener::bind(SocketAddr::new(
                                        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                                        0,
                                    ))
                                    .await?
                                }
                                SocketAddr::V6(_) => {
                                    TcpListener::bind(SocketAddr::new(
                                        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                                        0,
                                    ))
                                    .await?
                                }
                            };

                            Ok((connection_id, mirror_listener, local_address))
                        },
                    )
                    .await
                    .and_then(|(connection_id, listener, local_address)| {
                        // Incoming remote channel (in the direction of agent -> layer).
                        // When the layer gets data from the agent, it writes it in via the
                        // remote_tx end. the interceptor_task then reads
                        // the data via the remote_rx end and writes it to
                        // mirror_stream.
                        // Agent ----> layer --> remote_tx=====remote_rx --> interceptor -->
                        // mirror_stream
                        let (remote_tx, remote_rx) = channel::<Vec<u8>>(1000);

                        let _ = DetourGuard::new();
                        let mirror_address = listener.local_addr()?;

                        self.mirrors
                            .insert(connection_id, ConnectionMirror(remote_tx));

                        // `user` and `interceptor` sockets are not yet connected to each other,
                        // spawn a new task to `accept` the conncetion and pair their reads/writes.
                        task::spawn(TcpOutgoingHandler::interceptor_task(
                            self.layer_tx.clone(),
                            connection_id,
                            listener,
                            remote_rx,
                        ));

                        Ok(RemoteConnection {
                            local_address,
                            mirror_address,
                        })
                    });

                self.connect_queue
                    .pop_front()
                    .ok_or(LayerError::SendErrorTcpResponse)?
                    .send(response)
                    .map_err(|_| LayerError::SendErrorTcpResponse)
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

                sender.send(bytes).await.unwrap_or_else(|_| {
                    warn!(
                        "Got new data from agent after application closed socket. connection_id: \
                    connection_id: {connection_id}"
                    );
                });
                Ok(())
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
