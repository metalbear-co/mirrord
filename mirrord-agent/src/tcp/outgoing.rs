use std::{collections::HashMap, path::PathBuf};

use mirrord_protocol::{
    ConnectRequest, ConnectResponse, OutgoingTrafficRequest, OutgoingTrafficResponse, ReadResponse,
    RemoteResult, ResponseError, WriteRequest, WriteResponse,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::mpsc::{self, Receiver, Sender},
    task,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, trace, warn};

use crate::{error::AgentError, runtime::set_namespace};

type Request = OutgoingTrafficRequest;
type Response = OutgoingTrafficResponse;

pub(crate) struct OutgoingTrafficHandler {
    task: task::JoinHandle<Result<(), AgentError>>,
    request_channel_tx: Sender<Request>,
    response_channel_rx: Receiver<Response>,
}

#[derive(Debug)]
pub struct Data {
    id: i32,
    bytes: Vec<u8>,
}

impl OutgoingTrafficHandler {
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

    async fn interceptor_task(connection_id: i32, read_tx: Sender<Data>, mut stream: TcpStream) {
        trace!("OutgoingTrafficHandler::intercept_task -> ");

        let mut buffer = vec![0; 1500];

        loop {
            select! {
                biased;

                // Reads from the remote connection, then sends the data back to `layer` as a
                // `DaemonMessage`.
                read = stream.read(&mut buffer) => {
                    match read {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => {
                            error!("Failed reading stream with {:#?}", fail);
                            break;
                        }
                        Ok(read_amount) if read_amount == 0 => {
                            warn!("Local stream closed!");
                            break;
                        },
                        Ok(read_amount) => {
                            let bytes = buffer[..read_amount].to_vec();
                            let read = Data { id: connection_id, bytes };

                            if let Err(fail) = read_tx.send(read).await {
                                error!("Failed sending read message with {:#?}!", fail);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn inner_request_handler(
        read_tx: Sender<Data>,
        write_tx: Sender<Data>,
        request: Request,
        response_tx: Sender<Response>,
    ) -> Result<(), AgentError> {
        trace!(
            "OutgoingTrafficHandler::inner_request_handler -> request {:?}",
            request
        );

        match request {
            OutgoingTrafficRequest::Connect(ConnectRequest {
                user_fd,
                remote_address,
            }) => {
                let connect_response: RemoteResult<_> = TcpStream::connect(remote_address)
                    .await
                    .map_err(From::from)
                    .map(|remote_stream| {
                        task::spawn(Self::interceptor_task(user_fd, read_tx, remote_stream));

                        ConnectResponse { user_fd }
                    });

                trace!(
                    "OutgoingTrafficRequest::Connect -> connect_response {:#?}",
                    connect_response
                );

                let response = OutgoingTrafficResponse::Connect(connect_response);
                Ok(response_tx.send(response).await?)
            }
            OutgoingTrafficRequest::Write(WriteRequest { id, bytes }) => {
                // TODO(alex) [high] 2022-08-09: Now that we have a task per connection, this
                // doesn't work anymore. Must send a `Write` message + a `write_tx` channel to the
                // `intercept_task`, while we `recv` on `write_rx` here.
                Ok(write_tx.send(Data { id, bytes }).await?)
            }
        }
    }

    // TODO(alex) [high] 2022-08-09: Instead of holding a bunch of `TcpStream`s, just start a new
    // task per connection, similar to how it's being done in -layer. Then we can use `select!` on
    // `read` for the `TcpStream`.
    async fn run(
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

        let (read_tx, mut read_rx) = mpsc::channel(1000);
        let (write_tx, mut write_rx) = mpsc::channel(1000);

        loop {
            select! {
                // [layer] -> [agent]
                request = request_rx.recv() => {
                    match request {
                        Some(request) => {
                            OutgoingTrafficHandler::inner_request_handler(
                                read_tx.clone(),
                                write_tx.clone(),
                                request,
                                response_tx.clone(),
                            )
                            .await?
                        }
                        None => {
                            warn!("OutgoingTrafficHandler::run -> Disconnected!");
                            break;
                        }
                    }
                }
                // [remote] -> [layer]
                read = read_rx.recv() => {
                    if let Some(Data { id, bytes }) = read {
                        let read = ReadResponse { id, bytes };

                        let response = OutgoingTrafficResponse::Read(Ok(read));

                        response_tx.send(response).await?
                    }
                }

                // [?] -> [?]
                write = write_rx.recv() => {
                    if let Some(Data { id, bytes }) = write {
                        let write = WriteResponse {id, amount: todo!() };

                        let response = OutgoingTrafficResponse::Write(Ok(write));

                        response_tx.send(response).await?
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn handle_request(
        &mut self,
        request: OutgoingTrafficRequest,
    ) -> Result<OutgoingTrafficResponse, AgentError> {
        self.request_channel_tx.send(request).await?;

        self.response_channel_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}
