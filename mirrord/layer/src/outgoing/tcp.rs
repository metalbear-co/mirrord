use std::{
    collections::HashMap,
    env::temp_dir,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use futures::{FutureExt, TryFutureExt};
use mirrord_protocol::{
    outgoing::{
        tcp::*, DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, ConnectionId,
};
use rand::{distributions::Alphanumeric, Rng};
use socket2::{Domain, Socket, Type};
use tokio::{
    fs::create_dir_all,
    io,
    io::{copy_bidirectional, split, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf},
    net::{TcpStream, UnixStream},
    sync::mpsc::Sender,
    task,
};
use tokio_stream::{StreamExt, StreamMap};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, trace, warn};

use super::*;
use crate::{common::ResponseDeque, detour::DetourGuard, error::LayerError};

/// For unix socket addresses, relative to the temp dir (`/tmp/mirrord-bin/...`).
pub const UNIX_STREAMS_DIRNAME: &str = "mirrord-unix-sockets";

/// Hook messages handled by `TcpOutgoingHandler`.
///
/// - Part of [`HookMessage`](crate::common::HookMessage).
#[derive(Debug)]
pub(crate) enum TcpOutgoing {
    Connect(Connect),
}

/// Responsible for handling hook and daemon messages for the outgoing traffic feature.
#[derive(Debug, Default)]
pub(crate) struct TcpOutgoingHandler {
    // TODO: docs
    /// Holds the channels used to send daemon messages to the interceptor socket, for the case
    /// where (agent) received data from the remote host, and sent it to (layer), to finally be
    /// passed all the way back to the user.
    data_txs: HashMap<ConnectionId, WriteHalf<DuplexStream>>,

    // TODO: docs
    // TODO: if we want to send the forward shutdowns from the user to the agent, we can wrap the
    //      `StreamMap` with a `StreamNotifyClose`, and send a write message with an empty vec.
    data_rxs: StreamMap<ConnectionId, ReaderStream<ReadHalf<DuplexStream>>>,

    /// Holds the connection requests from the `connect` hook. It's main use is to reply back with
    /// the `SocketAddr` of the socket that'll be used to intercept the user's socket operations.
    connect_queue: ResponseDeque<RemoteConnection>,
}

impl TcpOutgoingHandler {
    // TODO: docs
    /// # Arguments
    ///
    /// * `layer_socket` - A bound and listening socket that the user application will connect to
    ///   (instead of to its actual remote target).
    /// * `remote_rx` - A channel for reading incoming data that was forwarded from the remote peer.
    #[tracing::instrument(level = "trace", skip_all)]
    async fn interceptor_task(layer_socket: Socket, mut data_stream: DuplexStream) {
        // Accepts the user's socket connection, and finally becomes the interceptor socket.
        let (mirror_stream, _) = layer_socket
            .accept()
            .inspect_err(|err| error!("{err}"))
            .unwrap();

        mirror_stream
            .set_nonblocking(true)
            .inspect_err(|err| error!("Error {err:?} when trying to set stream to nonblocking."))
            .unwrap();

        // We unwrap local_addr, because the socket must be bound at this point, so local_addr must
        // succeed.
        let local_addr = mirror_stream.local_addr().unwrap();
        if local_addr.is_unix() {
            let mut mirror_stream = UnixStream::from_std(mirror_stream.into())
                .map_err(|err| error!("Invalid unix stream socket address: {err:?}"))
                .unwrap();
            // TODO: eliminate duplication
            copy_bidirectional(&mut mirror_stream, &mut data_stream)
                .await
                .ok(); // TODO: handle error.
        } else if local_addr.is_ipv6() || local_addr.is_ipv4() {
            let mut mirror_stream = TcpStream::from_std(mirror_stream.into())
                .map_err(|err| error!("Invalid IP socket address: {err:?}"))
                .unwrap();
            copy_bidirectional(&mut mirror_stream, &mut data_stream)
                .await
                .ok(); // TODO: handle error.
        } else {
            error!("Unsupported socket address family, not intercepting.")
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

                let remote_address = remote_address.try_into()?;
                trace!("TCP Outgoing connection to {remote_address}.");
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

                            // Socket held by the layer, waiting for the user application to
                            // connect to it.
                            let layer_socket = match remote_address {
                                SocketAddress::Ip(SocketAddr::V4(_)) => {
                                    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
                                    socket.bind(
                                        &(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
                                            .into()),
                                    )?;
                                    socket
                                }
                                SocketAddress::Ip(SocketAddr::V6(_)) => {
                                    let socket = Socket::new(Domain::IPV6, Type::STREAM, None)?;
                                    socket.bind(
                                        &(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
                                            .into()),
                                    )?;
                                    socket
                                }
                                SocketAddress::Unix(_) => {
                                    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
                                    let tmp_dir = temp_dir().join(UNIX_STREAMS_DIRNAME);
                                    if !tmp_dir.exists() {
                                        create_dir_all(&tmp_dir).await?;
                                    }
                                    let random_string: String = rand::thread_rng()
                                        .sample_iter(&Alphanumeric)
                                        .take(16)
                                        .map(char::from)
                                        .collect();
                                    let pathname = tmp_dir.join(random_string);

                                    let addr = SockAddr::unix(pathname)?;
                                    // Could theoretically fail if we generated the same random
                                    // string for two connections, but that's rather unlikely.
                                    socket.bind(&addr)?;
                                    socket
                                }
                            };
                            // We are only accepting one connection on this socket, so setting
                            // backlog to 1.
                            layer_socket.listen(1)?;

                            Ok((connection_id, layer_socket, local_address.try_into()?))
                        },
                    )
                    .await
                    .and_then(|(connection_id, socket, user_app_address)| {
                        // TODO: update these docs.
                        // Incoming remote channel (in the direction of agent -> layer).
                        // When the layer gets data from the agent, it writes it in via the
                        // remote_tx end. the interceptor_task then reads
                        // the data via the remote_rx end and writes it to
                        // mirror_stream.
                        // Agent ----> layer --> remote_tx=====remote_rx --> interceptor -->
                        // mirror_stream

                        let (main_task_side, interceptor_task_side) = io::duplex(1024);
                        let (rx, tx) = split(main_task_side);
                        self.data_rxs.insert(connection_id, ReaderStream::new(rx));
                        self.data_txs.insert(connection_id, tx);

                        let _ = DetourGuard::new();
                        let layer_address = socket.local_addr()?;

                        // `user` and `interceptor` sockets are not yet connected to each other,
                        // spawn a new task to `accept` the connection and pair their reads/writes.
                        task::spawn(TcpOutgoingHandler::interceptor_task(
                            socket,
                            interceptor_task_side,
                        ));

                        Ok(RemoteConnection {
                            user_app_address,
                            layer_address,
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
                match read {
                    Ok(DaemonRead {
                        connection_id,
                        bytes,
                    }) => {
                        let writer = self
                            .data_txs
                            .get_mut(&connection_id)
                            .ok_or(LayerError::NoConnectionId(connection_id))?;

                        if bytes.is_empty() {
                            writer.shutdown().await.unwrap_or_else(|err| {
                                warn!(
                                    "Could not shutdown mirrord's writer to the outgoing \
                                    interceptor task. Error: {err}"
                                );
                            });
                        } else {
                            writer.write_all(&bytes[..]).await.unwrap_or_else(|_| {
                                warn!(
                                    "Got new data from agent after application closed socket. \
                                    connection_id: {connection_id}"
                                );
                            });
                        }
                    }
                    // We need connection id with these errors to actually do something
                    // pending protocol change
                    Err(e) => {
                        debug!("Remote socket disconnected {e:#?}")
                    }
                }

                Ok(())
            }
            DaemonTcpOutgoing::Close(connection_id) => {
                trace!("Close -> connection_id {:?}", connection_id);
                self.data_txs.remove(&connection_id);
                self.data_rxs.remove(&connection_id);
                Ok(())
            }
        }
    }

    /// Helper function to access the channel of messages that are to be passed directly as
    /// `ClientMessage` from `layer`.
    pub(crate) fn recv(&mut self) -> impl Future<Output = Option<LayerTcpOutgoing>> + '_ {
        self.data_rxs.next().map(|option| {
            option.map(|(connection_id, bytes)| {
                match bytes {
                    Err(err) => {
                        info!("Could not read outgoing data. Error: {:#?}", err);
                        LayerTcpOutgoing::Close(LayerClose { connection_id })
                    }
                    Ok(bytes) => {
                        // Sends the message that the user wrote to our interceptor socket to
                        // be handled on the `agent`, where it'll be forwarded to the remote.
                        LayerTcpOutgoing::Write(LayerWrite {
                            connection_id,
                            bytes: bytes.to_vec(),
                        })
                    }
                }
            })
        })
    }
}
