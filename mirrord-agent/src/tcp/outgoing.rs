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
use tracing::{trace, warn};

use crate::{error::AgentError, runtime::set_namespace};

type Request = OutgoingTrafficRequest;
type Response = OutgoingTrafficResponse;

pub(crate) struct OutgoingTrafficHandler {
    task: task::JoinHandle<Result<(), AgentError>>,
    request_channel_tx: Sender<Request>,
    response_channel_rx: Receiver<Response>,
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

    async fn inner_request_handler(
        request: Request,
        response_tx: Sender<Response>,
        remote_streams: &mut HashMap<i32, TcpStream>,
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
                        remote_streams.insert(user_fd, remote_stream);

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
                if let Some(stream) = remote_streams.get_mut(&id) {
                    let write_response: RemoteResult<_> = stream
                        .write(&bytes)
                        .await
                        .map_err(From::from)
                        .map(|written_amount| WriteResponse {
                            id,
                            amount: written_amount,
                        });

                    let response = OutgoingTrafficResponse::Write(write_response);
                    Ok(response_tx.send(response).await?)
                } else {
                    let response =
                        OutgoingTrafficResponse::Write(Err(ResponseError::NotFound(id as usize)));
                    Ok(response_tx.send(response).await?)
                }
            }
        }
    }

    // TODO(alex) [high] 2022-08-09: Instead of holding a bunch of `TcpStream`s, just start a new
    // task per connection, similar to how it's being done in -layer. Then we can use `select!` on
    // `read` for the `TcpStream`.
    async fn run(
        pid: Option<u64>,
        mut request_channel_rx: Receiver<Request>,
        response_channel_tx: Sender<Response>,
    ) -> Result<(), AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        let mut agent_remote_streams: HashMap<i32, TcpStream> = HashMap::with_capacity(4);
        let mut read_buffer = vec![0; 1500];

        loop {
            select! {
                request = request_channel_rx.recv() => {
                    match request {
                        Some(request) => OutgoingTrafficHandler::inner_request_handler(request, response_channel_tx.clone(), &mut agent_remote_streams).await?,
                        None => {
                            warn!("OutgoingTrafficHandler::run -> Disconnected!");
                            break;
                        }
                    }
                } else => {
                    for (id, stream) in agent_remote_streams.iter_mut() {
                        let read_response: RemoteResult<_> = stream
                            .read(&mut read_buffer)
                            .await
                            .map_err(From::from)
                            .map(|read_amount| ReadResponse {
                                id: *id,
                                bytes: read_buffer[..read_amount].to_vec(),
                            });

                        let response = OutgoingTrafficResponse::Read(read_response);
                        response_channel_tx.send(response).await.unwrap();
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
