use std::{
    collections::HashMap,
    env::temp_dir,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures::TryFutureExt;
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
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UnixStream},
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
    time::sleep,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::{error, info, trace, warn};

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
    /// # Arguments
    ///
    /// * `layer_tx` - A channel for sending data from the local app to the main task (to be
    ///   forwarded to the agent).
    /// * `layer_socket` - A bound and listening socket that the user application will connect to
    ///   (instead of to its actual remote target).
    /// * `remote_rx` - A channel for reading incoming data that was forwarded from the remote peer.
    #[tracing::instrument(level = "trace", skip(layer_tx, layer_socket, remote_rx))]
    async fn interceptor_task(
        layer_tx: Sender<LayerTcpOutgoing>,
        connection_id: ConnectionId,
        layer_socket: Socket,
        remote_rx: Receiver<Vec<u8>>,
    ) {
        let remote_stream = ReceiverStream::new(remote_rx);

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
            let mirror_stream = UnixStream::from_std(mirror_stream.into())
                .map_err(|err| error!("Invalid unix stream socket address: {err:?}"))
                .unwrap();
            Self::forward_bidirectionally(layer_tx, connection_id, mirror_stream, remote_stream)
                .await;
        } else if local_addr.is_ipv6() || local_addr.is_ipv4() {
            let mirror_stream = TcpStream::from_std(mirror_stream.into())
                .map_err(|err| error!("Invalid IP socket address: {err:?}"))
                .unwrap();
            Self::forward_bidirectionally(layer_tx, connection_id, mirror_stream, remote_stream)
                .await;
        } else {
            error!("Unsupported socket address family, not intercepting.")
        }
    }

    /// Forward in both directions data between the user application and the main layer task (that
    /// forwards it to the agent, that sends it to the remote target).
    ///
    /// ```
    ///      layer_tx ◄──────
    ///                     layer_to_user_stream ◄───────► user app
    /// remote_stream ───────►
    /// ```
    ///
    /// # Arguments
    ///
    /// * `layer_tx` - A channel for sending data from the local app to the main layer task (to be
    ///   forwarded to the agent).
    /// * `layer_to_user_stream` - An outgoing stream (could be UNIX, TCP/IP, or anything that's
    ///   async read and write) between the user app and the layer.
    /// * `remote_stream` - A [`ReceiverStream`] to read the response data forwarded from the agent
    ///   (to be forwarded to `layer_to_user_stream`).
    async fn forward_bidirectionally<T: AsyncRead + AsyncWrite + Unpin>(
        layer_tx: Sender<LayerTcpOutgoing>,
        connection_id: ConnectionId,
        mut layer_to_user_stream: T,
        mut remote_stream: ReceiverStream<Vec<u8>>,
    ) {
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

        let mut buffer = vec![0; 1024];
        let mut remote_stream_closed = false;

        loop {
            select! {
                biased; // To allow local socket to be read before being closed

                // Reads data that the user is sending from their socket to mirrord's interceptor
                // socket.
                read = layer_to_user_stream.read(&mut buffer) => {
                    match read {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => {
                            trace!("Failed reading mirror_stream with {:#?}", fail);
                            if !remote_stream_closed {
                                close_remote_stream(layer_tx.clone()).await;
                            }

                            break;
                        }
                        Ok(read_amount) if read_amount == 0 => {
                            trace!("interceptor_task -> Stream {:#?} has no more data, closing!", connection_id);
                            if !remote_stream_closed {
                                close_remote_stream(layer_tx.clone()).await;
                            }
                            break;
                        },
                        Ok(read_amount) => {
                            // Sends the message that the user wrote to our interceptor socket to
                            // be handled on the `agent`, where it'll be forwarded to the remote.
                            let write = LayerWrite {
                                connection_id,
                                bytes: buffer
                                    .get(..read_amount)
                                    .expect("read returned more bytes than the buffer can hold")
                                    .to_vec(),
                            };
                            let outgoing_write = LayerTcpOutgoing::Write(write);

                            if let Err(fail) = layer_tx.send(outgoing_write).await {
                                error!("Failed sending write message with {:#?}!", fail);

                                break;
                            }
                        }
                    }
                },
                bytes = remote_stream.next(), if !remote_stream_closed  => {
                    match bytes {
                        Some(bytes) => {
                            // Writes the data sent by `agent` (that came from the actual remote
                            // stream) to our interceptor socket. When the user tries to read the
                            // remote data, this'll be what they receive.
                            if let Err(fail) = layer_to_user_stream.write_all(&bytes).await {
                                trace!("Failed writing to mirror_stream with {:#?}!", fail);
                                break;
                            }
                        },
                        None => {
                            warn!("interceptor_task -> shutdown due to remote stream closed!");
                            // remote socket closed --> shutdown local socket
                            if let Err(err) = layer_to_user_stream.shutdown().await {
                                warn!("Failed shutting down mirror_stream with {:#?}!", err);
                            }
                            remote_stream_closed = true;
                        }
                    }
                }
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
                        // Incoming remote channel (in the direction of agent -> layer).
                        // When the layer gets data from the agent, it writes it in via the
                        // remote_tx end. the interceptor_task then reads
                        // the data via the remote_rx end and writes it to
                        // mirror_stream.
                        // Agent ----> layer --> remote_tx=====remote_rx --> interceptor -->
                        // mirror_stream
                        let (remote_tx, remote_rx) = channel::<Vec<u8>>(1000);

                        let _ = DetourGuard::new();
                        let layer_address = socket.local_addr()?;

                        self.mirrors
                            .insert(connection_id, ConnectionMirror(remote_tx));

                        // `user` and `interceptor` sockets are not yet connected to each other,
                        // spawn a new task to `accept` the conncetion and pair their reads/writes.
                        task::spawn(TcpOutgoingHandler::interceptor_task(
                            self.layer_tx.clone(),
                            connection_id,
                            socket,
                            remote_rx,
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
                    }
                    Err(e) => {
                        // on errors, we need to end the interceptor task, however this would
                        // require introducing some breaking changes to the protocol
                        // on hold because of: https://github.com/metalbear-co/mirrord/issues/1353
                        // some solutions (AH):
                        // - add `DaemonTcpOutgoing::Error(errno)` and then pass it to the
                        //   `interceptor_task` to break.
                        // - change `DaemonTcpOutgoing::Close` to `Close(Option<errno>)` and pass it
                        //   to the `interceptor_task` to know when to shutdown and when to break
                        //   loop
                        warn!("Read error: {:?}", e);
                    }
                }
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
