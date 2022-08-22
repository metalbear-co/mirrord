use core::fmt;
use std::{collections::HashMap, path::PathBuf, thread};

use mirrord_protocol::{tcp::outgoing::*, ConnectionId, RemoteResult, ResponseError};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    select,
    sync::mpsc::{self, Receiver, Sender},
    task,
};
use tokio_stream::{StreamExt, StreamMap};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, trace, warn};

use crate::{error::AgentError, runtime::set_namespace, util::run_thread};

type Request = TcpOutgoingRequest;
type Response = TcpOutgoingResponse;

// TODO(alex) [high] 2022-08-19: Instead of spawning a bunch of tasks, use a `StreamMap` to deal
// with multiple connections!
pub(crate) struct TcpOutgoingApi {
    task: thread::JoinHandle<Result<(), AgentError>>,
    request_tx: Sender<Request>,
    response_rx: Receiver<Response>,
}

pub struct Data {
    connection_id: ConnectionId,
    bytes: Vec<u8>,
}

impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Data")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

impl TcpOutgoingApi {
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (request_tx, request_rx) = mpsc::channel(1000);
        let (response_tx, response_rx) = mpsc::channel(1000);

        // let task = task::spawn(Self::interceptor_task(pid, request_rx, response_tx));
        let task = run_thread(Self::interceptor_task(pid, request_rx, response_tx));

        Self {
            task,
            request_tx,
            response_rx,
        }
    }

    async fn interceptor_task(
        pid: Option<u64>,
        mut request_rx: Receiver<Request>,
        response_tx: Sender<Response>,
    ) -> Result<(), AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        let mut writers: HashMap<ConnectionId, OwnedWriteHalf> = HashMap::default();
        let mut readers: StreamMap<ConnectionId, ReaderStream<OwnedReadHalf>> =
            StreamMap::default();

        loop {
            select! {
                biased;

                // [layer] -> [agent]
                request = request_rx.recv() => {
                    trace!("interceptor_task -> request {:?}", request);

                    match request {
                        Some(request) => {
                            match request {
                                // [layer] -> [agent] -> [layer]
                                TcpOutgoingRequest::Connect(ConnectRequest { remote_address }) => {
                                    let connect_response =
                                        TcpStream::connect(remote_address)
                                            .await
                                            .map_err(From::from)
                                            .map(|remote_stream| {
                                                let connection_id = writers
                                                    .keys()
                                                    .last()
                                                    .copied()
                                                    .map(|last| last + 1)
                                                    .unwrap_or_default();

                                                let (read_half, write_half) = remote_stream.into_split();
                                                writers.insert(connection_id, write_half);
                                                readers.insert(connection_id, ReaderStream::new(read_half));

                                                ConnectResponse {
                                                    connection_id,
                                                    remote_address,
                                                }
                                            });

                                    let response = TcpOutgoingResponse::Connect(connect_response);
                                    response_tx.send(response).await?
                                }
                                // [layer] -> [agent] -> [remote]
                                TcpOutgoingRequest::Write(WriteRequest {
                                    connection_id,
                                    bytes,
                                }) => {
                                    let write_response = match writers
                                        .get_mut(&connection_id)
                                        .ok_or(ResponseError::NotFound(connection_id as usize))
                                    {
                                        Ok(writer) => writer
                                            .write_all(&bytes)
                                            .await
                                            .map_err(ResponseError::from)
                                            .map(|()| WriteResponse { connection_id }),
                                        Err(fail) => Err(fail),
                                    };

                                    let response = TcpOutgoingResponse::Write(write_response);
                                    response_tx.send(response).await?
                                }

                                TcpOutgoingRequest::Close(CloseRequest { connection_id} ) => {
                                    writers.remove(&connection_id);
                                    readers.remove(&connection_id);

                                    if writers.len() == 0 && readers.len() == 0 {
                                        break;
                                    }
                                }
                            }
                        }
                        None => {
                            warn!("run -> Disconnected!");
                            break;
                        }
                    }
                }

                // [remote] -> [agent] -> [layer]
                Some((connection_id, value)) = readers.next() => {
                    trace!("interceptor_task -> read connection_id {:#?}", connection_id);

                    let read_response = value
                        .map_err(ResponseError::from)
                        .map(|bytes| ReadResponse { connection_id, bytes: bytes.to_vec() });

                    let response = TcpOutgoingResponse::Read(read_response);
                    response_tx.send(response).await?

                }
                else => {
                    trace!("interceptor_task -> no messages left");
                    break;
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn request(&mut self, request: TcpOutgoingRequest) -> Result<(), AgentError> {
        trace!("TcpOutgoingApi::request -> request {:#?}", request);
        Ok(self.request_tx.send(request).await?)
    }

    pub(crate) async fn response(&mut self) -> Result<TcpOutgoingResponse, AgentError> {
        self.response_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}
