use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    os::unix::prelude::AsRawFd,
    path::PathBuf,
};

use actix_codec::Framed;
use futures::{
    future::TryFutureExt,
    stream::{FuturesUnordered, StreamExt},
    SinkExt,
};
use mirrord_protocol::{
    tcp::LayerTcp, AddrInfoHint, AddrInfoInternal, ClientMessage, ConnectRequest, ConnectResponse,
    DaemonCodec, DaemonMessage, GetAddrInfoRequest, GetEnvVarsRequest, OutgoingTrafficRequest,
    OutgoingTrafficResponse, ReadRequest, ReadResponse, RemoteResult, ResponseError, WriteRequest,
    WriteResponse,
};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Receiver, Sender},
    task,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};
use tracing_subscriber::prelude::*;

use crate::{
    error::AgentError,
    runtime::{get_container_pid, set_namespace},
};

type Request = OutgoingTrafficRequest;
type Response = OutgoingTrafficResponse;

pub(crate) struct OutgoingTcpHandler {
    task: task::JoinHandle<()>,
    request_channel_tx: Sender<Request>,
    response_channel_rx: Receiver<Response>,
}

impl OutgoingTcpHandler {
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (request_channel_tx, request_channel_rx) = mpsc::channel(1000);
        let (response_channel_tx, response_channel_rx) = mpsc::channel(1000);

        let task = task::spawn(Self::run(pid, request_channel_rx, response_channel_tx));

        Self {
            task,
            request_channel_tx,
            response_channel_rx,
        }
    }

    async fn run(
        pid: Option<u64>,
        mut request_channel_rx: Receiver<Request>,
        response_channel_tx: Sender<Response>,
    ) {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        let mut agent_remote_streams: HashMap<i32, TcpStream> = HashMap::with_capacity(4);
        let mut read_buffer = vec![0; 1500];

        loop {
            if let Some(request) = request_channel_rx.recv().await {
                match request {
                    OutgoingTrafficRequest::Connect(ConnectRequest { remote_address }) => {
                        let connect_response: RemoteResult<_> = TcpStream::connect(remote_address)
                            .await
                            .map_err(From::from)
                            .and_then(|remote_stream| {
                                agent_remote_streams.insert(1, remote_stream);

                                Ok(ConnectResponse)
                            });

                        let response = OutgoingTrafficResponse::Connect(connect_response);
                        response_channel_tx.send(response).await.unwrap();
                    }
                    OutgoingTrafficRequest::Read(ReadRequest { id }) => {
                        if let Some(stream) = agent_remote_streams.get_mut(&id) {
                            let read_response: RemoteResult<_> = stream
                                .read(&mut read_buffer)
                                .await
                                .map_err(From::from)
                                .and_then(|read_amount| {
                                    Ok(ReadResponse {
                                        id,
                                        bytes: read_buffer[..read_amount].to_vec(),
                                    })
                                });

                            let response = OutgoingTrafficResponse::Read(read_response);
                            response_channel_tx.send(response).await.unwrap();
                        } else {
                            let response = OutgoingTrafficResponse::Read(Err(
                                ResponseError::NotFound(id as usize),
                            ));

                            response_channel_tx.send(response).await.unwrap();
                        }
                    }
                    OutgoingTrafficRequest::Write(WriteRequest { id, bytes }) => {
                        if let Some(stream) = agent_remote_streams.get_mut(&id) {
                            let write_response: RemoteResult<_> =
                                stream.write(&bytes).await.map_err(From::from).and_then(
                                    |written_amount| {
                                        Ok(WriteResponse {
                                            id,
                                            amount: written_amount,
                                        })
                                    },
                                );

                            let response = OutgoingTrafficResponse::Write(write_response);
                            response_channel_tx.send(response).await.unwrap();
                        } else {
                            let response = OutgoingTrafficResponse::Write(Err(
                                ResponseError::NotFound(id as usize),
                            ));
                            response_channel_tx.send(response).await.unwrap();
                        }
                    }
                }
            }

            for (id, remote_stream) in agent_remote_streams.iter_mut() {}

            // TODO(alex) [high] 2022-07-19:
            // 1. Loop through `agent_remote_streams` for `recv`;
            // 2. Send the data back as `DaemonMessage::OutgoingTraffic(Recv)` to the respective
            // `mirror_socket` (layer <-> agent connection);
            // 3. layer reads a `DaemonMessage` and triggers a call to
            // `outgoing_traffic_handler.recv(data)`;
            // 4. It sends a message from `mirror_socket` to `user_socket`;
            //
            // Something similar must be done for `send`.

            // if let Some(ConnectRequest { remote_address }) = request_channel_rx.recv().await {
            //     let connect_result: RemoteResult<_> =
            //         TcpStream::connect(remote_address).await.map_err(From::from);

            //     match connect_result {
            //         Ok(tcp_stream) => {
            //             let fd = tcp_stream.as_raw_fd();
            //             agent_remote_streams.insert(fd, tcp_stream);

            //             let bind_result: RemoteResult<_> =
            //                 TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST),
            // 0))                     .await
            //                     .map_err(From::from);

            //             match bind_result {
            //                 Ok(listener) => {
            //                     let mirror_address = listener.local_addr().map_err(From::from);

            //                     match mirror_address {
            //                         Ok(mirror_address) => {
            //                             let response = ConnectResponse;

            //                             response_channel_tx.send(Ok(response)).await;

            //                             let accept_result =
            //                                 listener.accept().await.map_err(From::from);

            //                             match accept_result {
            //                                 Ok((stream, address)) => {
            //                                     let fd = stream.as_raw_fd();
            //                                     layer_agent_streams.insert(fd, stream);
            //                                 }
            //                                 Err(fail) => {
            //                                     response_channel_tx.send(Err(fail)).await;
            //                                 }
            //                             }
            //                         }
            //                         Err(fail) => {
            //                             response_channel_tx.send(Err(fail)).await;
            //                         }
            //                     }
            //                 }
            //                 Err(fail) => {
            //                     response_channel_tx.send(Err(fail)).await;
            //                 }
            //             }
            //         }
            //         Err(fail) => {
            //             response_channel_tx.send(Err(fail)).await;
            //         }
            //     };
            // }

            // for (_, layer_agent_stream) in layer_agent_streams.iter_mut() {
            //     let read = layer_agent_stream.read(&mut read_buffer).await;
            //     debug!(
            //         "OutgoingTcpHandler::run -> layer_agent_stream::read {:#?}",
            //         read
            //     );
            //     // TODO(alex) [high] 2022-07-18: Gotta send the message we just read to the
            // remote     // stream, this means we need an association of `layer` to
            // `remote` (the `fd` alone     // as the key for the map is not enough).
            // }

            // for (_, agent_remote_stream) in agent_remote_streams.iter_mut() {
            //     let read = agent_remote_stream.read(&mut read_buffer).await;
            //     debug!(
            //         "OutgoingTcpHandler::run -> agent_remote_stream::read {:#?}",
            //         read
            //     );
            // }
        }
    }

    async fn connect(&mut self, request: ConnectRequest) -> Result<(), AgentError> {
        // self.request_channel_tx.send(request).await?;

        // TODO(alex) [high] 2202-07-18: Receive the address that we're going to send back to layer.
        self.response_channel_rx.recv();

        Ok(todo!())
    }

    pub(crate) fn handle_message(
        &self,
        request: OutgoingTrafficRequest,
    ) -> Result<OutgoingTrafficResponse, AgentError> {
        todo!()
    }
}
