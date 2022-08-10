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
use tracing::{debug, error, trace, warn};

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

    async fn interceptor_task(
        connection_id: i32,
        response_tx: Sender<Response>,
        mut write_rx: Receiver<Data>,
        stream: TcpStream,
    ) {
        trace!(
            "OutgoingTrafficHandler::intercept_task -> id {:#?}",
            connection_id
        );

        let mut buffer = vec![0; 1500];

        let read_response_tx = response_tx.clone();
        let (mut read_stream, mut write_stream) = stream.into_split();

        let read_task = tokio::spawn(async move {
            let read = read_stream.read(&mut buffer).await;
            trace!("interceptor_task -> read {:#?}", read);

            match read {
                Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(fail) => {
                    panic!("Failed reading stream with {:#?}", fail);
                }
                Ok(read_amount) if read_amount == 0 => {
                    panic!("Local stream closed!");
                }
                Ok(read_amount) => {
                    let bytes = buffer[..read_amount].to_vec();
                    let read = ReadResponse {
                        id: connection_id,
                        bytes,
                    };
                    let response = OutgoingTrafficResponse::Read(Ok(read));

                    if let Err(fail) = read_response_tx.send(response).await {
                        panic!("Failed sending read message with {:#?}!", fail);
                    }
                }
            }
        });

        loop {
            match write_rx.recv().await {
                Some(data) => {
                    trace!("interceptor_task -> write has data {:?}", data.id);
                    let written = write_stream.write_all(&data.bytes).await;
                    match written {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        }
                        Err(fail) => {
                            error!("Failed writing stream with {:#?}", fail);
                            break;
                        }
                        Ok(()) => {
                            debug!("interceptor_task -> write ok!");
                            let write = WriteResponse {
                                id: connection_id,
                                amount: 1,
                            };
                            let response = OutgoingTrafficResponse::Write(Ok(write));

                            if let Err(fail) = response_tx.send(response).await {
                                error!("Failed sending read message with {:#?}!", fail);
                                break;
                            }
                        }
                    }
                }
                None => {
                    error!("Write channel closed {:#?}!", connection_id);
                    break;
                }
            }

            // select! {
            //     // [layer] -> [remote]
            //     write = write_rx.recv() => {
            //         match write {
            //             Some(data) => {
            //                 // TODO(alex) [high] 2022-08-10: Why don't we get here?
            //                 trace!("interceptor_task -> write has data {:?}", data.id);
            //                 let written = write_stream.write_all(&data.bytes).await;
            //                 match written {
            //                     Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
            //                         continue;
            //                     },
            //                     Err(fail) => {
            //                         error!("Failed writing stream with {:#?}", fail);
            //                         break;
            //                     }
            //                     Ok(()) => {
            //                         debug!("interceptor_task -> write ok!");
            //                         let write = WriteResponse { id: connection_id, amount: 1 };
            //                         let response = OutgoingTrafficResponse::Write(Ok(write));

            //                         if let Err(fail) = response_tx.send(response).await {
            //                             error!("Failed sending read message with {:#?}!", fail);
            //                             break;
            //                         }
            //                     }
            //                 }
            //             }
            //             None => continue,
            //         }
            //     }

            //     // Reads from the remote connection, then sends the data back to `layer` as a
            //     // `DaemonMessage`.
            //     // [remote] -> [layer]
            //     // read = stream.read(&mut buffer) => {
            //     //     trace!("interceptor_task -> read {:#?}", read);

            //     //     match read {
            //     //         Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
            //     //             continue;
            //     //         },
            //     //         Err(fail) => {
            //     //             error!("Failed reading stream with {:#?}", fail);
            //     //             break;
            //     //         }
            //     //         Ok(read_amount) if read_amount == 0 => {
            //     //             warn!("Local stream closed!");
            //     //             break;
            //     //         },
            //     //         Ok(read_amount) => {
            //     //             let bytes = buffer[..read_amount].to_vec();
            //     //             let read = ReadResponse { id: connection_id, bytes };
            //     //             let response = OutgoingTrafficResponse::Read(Ok(read));

            //     //             if let Err(fail) = response_tx.send(response).await {
            //     //                 error!("Failed sending read message with {:#?}!", fail);
            //     //                 break;
            //     //             }
            //     //         }
            //     //     }
            //     // }
            // }
        }
    }

    async fn inner_request_handler(
        request: Request,
        response_tx: Sender<Response>,
    ) -> Result<(), AgentError> {
        trace!(
            "OutgoingTrafficHandler::inner_request_handler -> request {:?}",
            request
        );

        let (write_tx, write_rx) = mpsc::channel(1000);

        match request {
            OutgoingTrafficRequest::Connect(ConnectRequest {
                user_fd,
                remote_address,
            }) => {
                let connect_response: RemoteResult<_> = TcpStream::connect(remote_address)
                    .await
                    .map_err(From::from)
                    .map(|remote_stream| {
                        task::spawn(Self::interceptor_task(
                            user_fd,
                            response_tx.clone(),
                            write_rx,
                            remote_stream,
                        ));

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
                trace!("OutgoingTrafficRequest::Write -> write_request {:#?}", id);
                // TODO(alex) [high] 2022-08-09: Now that we have a task per connection, this
                // doesn't work anymore. Must send a `Write` message + a `write_tx` channel to the
                // `intercept_task`, while we `recv` on `write_rx` here.
                let write = Data { id, bytes };
                debug!("before write!");
                let write_debug = write_tx.send(write).await?;
                debug!("after write!");

                Ok(write_debug)
            }
        }
    }

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

        let mut senders: HashMap<i32, Sender<Data>> = HashMap::with_capacity(4);

        loop {
            // [layer] -> [agent]
            match request_rx.recv().await {
                Some(request) => {
                    // OutgoingTrafficHandler::inner_request_handler(request, response_tx.clone())
                    //     .await?

                    trace!(
                        "OutgoingTrafficHandler::inner_request_handler -> request {:?}",
                        request
                    );

                    match request {
                        OutgoingTrafficRequest::Connect(ConnectRequest {
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

                            trace!(
                                "OutgoingTrafficRequest::Connect -> connect_response {:#?}",
                                connect_response
                            );

                            let response = OutgoingTrafficResponse::Connect(connect_response);
                            response_tx.send(response).await?
                        }
                        OutgoingTrafficRequest::Write(WriteRequest { id, bytes }) => {
                            trace!("OutgoingTrafficRequest::Write -> write_request {:#?}", id);

                            let write = Data { id, bytes };
                            senders.get(&id).unwrap().send(write).await?
                        }
                    }
                }
                None => {
                    warn!("OutgoingTrafficHandler::run -> Disconnected!");
                    break;
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
