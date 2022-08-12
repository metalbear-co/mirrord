use core::fmt;
use std::{collections::HashMap, path::PathBuf};

use mirrord_protocol::{
    tcp::outgoing::{
        ConnectRequest, ConnectResponse, ReadResponse, TcpOutgoingRequest, TcpOutgoingResponse,
        WriteRequest, WriteResponse,
    },
    RemoteResult,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::mpsc::{self, Receiver, Sender},
    task,
};
use tracing::{debug, error, trace, warn};

use crate::{error::AgentError, runtime::set_namespace};

type Request = TcpOutgoingRequest;
type Response = TcpOutgoingResponse;

pub(crate) struct TcpOutgoingApi {
    task: task::JoinHandle<Result<(), AgentError>>,
    request_channel_tx: Sender<Request>,
    response_channel_rx: Receiver<Response>,
}

pub struct Data {
    id: i32,
    bytes: Vec<u8>,
}

impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Data")
            .field("id", &self.id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

impl TcpOutgoingApi {
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (request_channel_tx, request_channel_rx) = mpsc::channel(1000);
        let (response_channel_tx, response_channel_rx) = mpsc::channel(1000);

        let task = task::spawn(Self::request_task(
            pid,
            request_channel_rx,
            response_channel_tx,
        ));

        Self {
            task,
            request_channel_tx,
            response_channel_rx,
        }
    }

    async fn interceptor_task(
        connection_id: i32,
        response_tx: Sender<Response>,
        mut write_rx: Receiver<Data>,
        stream: TcpStream,
    ) {
        trace!("intercept_task -> id {:#?}", connection_id);

        let mut buffer = vec![0; 1500];
        let (remote_reader, mut remote_writer) = stream.into_split();

        loop {
            select! {
                biased;

                readable = remote_reader.readable() => {
                    match readable {
                        Ok(_) => {
                            match remote_reader.try_read(&mut buffer) {
                                Ok(read_amount) if read_amount == 0 => {
                                    warn!("intercept_task -> Read stream is closed!");
                                    break;
                                }
                                Ok(read_amount) => {
                                    let bytes = buffer[..read_amount].to_vec();
                                    let read = ReadResponse {
                                        id: connection_id,
                                        bytes,
                                    };
                                    let response = TcpOutgoingResponse::Read(Ok(read));

                                    if let Err(fail) = response_tx.send(response).await {
                                        error!("intercept_task -> Failed sending response with {:#?}", fail);
                                        break;
                                    }
                                }
                                Err(ref fail) if fail.kind() == std::io::ErrorKind::WouldBlock => continue,
                                Err(fail) => {
                                    error!("Failed reading stream with {:#?}", fail);
                                    break;
                                }
                            }
                        }
                        Err(fail) => {
                            error!("Failed remote_reader readable() with {:#?}", fail);
                            break;
                        }
                    }
                }

                write = write_rx.recv() => {
                    match write {
                        Some(data) => {
                            let result = remote_writer.write_all(&data.bytes).await;

                            match result {
                                Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => continue,
                                Err(fail) => {
                                    error!("Failed writing stream with {:#?}", fail);
                                    break;
                                }
                                Ok(()) => {
                                    debug!("interceptor_task -> write ok!");
                                    let write = WriteResponse {
                                        id: connection_id,
                                    };
                                    let response = TcpOutgoingResponse::Write(Ok(write));

                                    if let Err(fail) = response_tx.send(response).await {
                                        error!("Failed sending read message with {:#?}!", fail);
                                        break;
                                    }
                                }
                            }
                        }
                        None => {
                            warn!("intercept_task-> write_rx closed {:#?}!", connection_id);
                            break;
                        }
                    }
                }
            }
        }

        trace!("intercept_task -> Finished id {:#?}", connection_id);
    }

    async fn request_task(
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

        let mut senders: HashMap<i32, Sender<Data>> = HashMap::with_capacity(4);

        loop {
            // [layer] -> [agent]
            match request_rx.recv().await {
                Some(request) => {
                    trace!("inner_request_handler -> request {:?}", request);

                    match request {
                        TcpOutgoingRequest::Connect(ConnectRequest {
                            user_fd,
                            remote_address,
                        }) => {
                            let connect_response: RemoteResult<_> =
                                TcpStream::connect(remote_address)
                                    .await
                                    .map_err(From::from)
                                    .map(|remote_stream| {
                                        let (write_tx, write_rx) = mpsc::channel(1000);

                                        senders.insert(user_fd, write_tx.clone());

                                        task::spawn(Self::interceptor_task(
                                            user_fd,
                                            response_tx.clone(),
                                            write_rx,
                                            remote_stream,
                                        ));

                                        ConnectResponse { user_fd }
                                    });

                            trace!("Connect -> response {:#?}", connect_response);

                            let response = TcpOutgoingResponse::Connect(connect_response);
                            response_tx.send(response).await?
                        }
                        TcpOutgoingRequest::Write(WriteRequest { id, bytes }) => {
                            trace!("Write -> request {:#?}", id);

                            let write = Data { id, bytes };
                            senders.get(&id).unwrap().send(write).await?
                        }
                    }
                }
                None => {
                    warn!("run -> Disconnected!");
                    break;
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn request(&mut self, request: TcpOutgoingRequest) -> Result<(), AgentError> {
        Ok(self.request_channel_tx.send(request).await?)
    }

    pub(crate) async fn response(&mut self) -> Result<TcpOutgoingResponse, AgentError> {
        self.response_channel_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}
