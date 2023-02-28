use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    client::conn::http1::{handshake, SendRequest},
    StatusCode,
};
use mirrord_protocol::{
    tcp::{
        Filter, HttpRequest, HttpResponse, LayerTcpSteal, NewTcpConnection,
        StealType::{All, FilteredHttp},
        TcpClose, TcpData,
    },
    ClientMessage, ConnectionId, Port,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::{error, info, trace, warn};

use crate::{
    error::LayerError,
    tcp::{Listen, TcpHandler},
};

pub(crate) mod http_forwarding;

use crate::{detour::DetourGuard, tcp_steal::http_forwarding::HttpForwarderError};

pub struct TcpStealHandler {
    ports: HashSet<Listen>,
    write_streams: HashMap<ConnectionId, WriteHalf<TcpStream>>,
    read_streams: StreamMap<ConnectionId, ReaderStream<ReadHalf<TcpStream>>>,

    /// Mapping of a ConnectionId to a sender that sends HTTP requests over to a task that is
    /// running an http client for this connection.
    http_request_senders: HashMap<ConnectionId, Sender<HttpRequest>>,

    /// Sender of responses from within an http client task back to the main layer task.
    /// This sender is cloned and moved into those tasks.
    http_response_sender: Sender<HttpResponse>,

    /// A string with a header regex to filter HTTP requests by.
    http_filter: Option<String>,

    /// These ports would be filtered with the `http_filter`, if it's Some.
    http_ports: Vec<u16>,

    /// LocalPort:RemotePort mapping.
    port_mapping: HashMap<u16, u16>,
}

#[async_trait]
impl TcpHandler for TcpStealHandler {
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_new_connection(
        &mut self,
        tcp_connection: NewTcpConnection,
    ) -> Result<(), LayerError> {
        let stream = self.create_local_stream(&tcp_connection).await?;

        let (read_half, write_half) = tokio::io::split(stream);
        self.write_streams
            .insert(tcp_connection.connection_id, write_half);
        self.read_streams
            .insert(tcp_connection.connection_id, ReaderStream::new(read_half));

        Ok(())
    }

    /// Forward incoming data from the agent to the local app.
    /// Browser -> agent -> layer -> local-app
    ///                           ^-- You are here.
    /// # Errors
    /// If the local connection with the app was closed, returns a
    /// [`LayerError::AppClosedConnection`] error, which contains a message to send to the agent
    /// to inform it of the close.
    #[tracing::instrument(level = "trace", skip(self), fields(data = data.connection_id))]
    async fn handle_new_data(&mut self, data: TcpData) -> Result<(), LayerError> {
        let connection = if let Some(conn) = self.write_streams.get_mut(&data.connection_id) {
            conn
        } else {
            trace!(
                "mirrord got new stolen incoming tcp data for a connection that is already closed: \
                {:?}",
                data.connection_id,
            );
            return Ok(());
        };

        trace!(
            "handle_new_data -> writing {:#?} bytes to id {:#?}",
            data.bytes.len(),
            data.connection_id
        );

        // Returns AppClosedConnection Error with message to send to agent if this fails.
        let _ = connection.write_all(&data.bytes[..]).await.map_err(|err| {
            trace!(
                "mirrord could not forward all the incoming data in connection id {}. \
                    Got error: {:?}",
                data.connection_id,
                err
            );
            LayerError::AppClosedConnection(self.app_closed_connection(data.connection_id))
        })?;

        Ok(())
    }

    /// An http request was stolen by the http filter. Pass it to the local application.
    ///
    /// Send a filtered HTTP request to the application in the appropriate port.
    /// If this is the first filtered HTTP from its remote connection to arrive at this layer, a new
    /// local connection will be started for it, otherwise it will be sent in the existing local
    /// connection.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_http_request(&mut self, request: HttpRequest) -> Result<(), LayerError> {
        if let Some(sender) = self.http_request_senders.get(&request.connection_id) {
            trace!(
                "Got an HTTP request from an existing connection, sending it to the client task \
                to be forwarded to the application."
            );
            Ok(sender
                .send(request)
                .await
                .map_err::<HttpForwarderError, _>(From::from)?)
        } else {
            Ok(self.create_http_connection(request).await?)
        }
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn handle_close(&mut self, close: TcpClose) -> Result<(), LayerError> {
        let TcpClose { connection_id } = close;

        // Dropping the connection -> Sender drops -> Receiver disconnects -> tcp_tunnel ends
        self.remove_connection(connection_id);

        Ok(())
    }

    fn ports(&self) -> &HashSet<Listen> {
        &self.ports
    }

    fn ports_mut(&mut self) -> &mut HashSet<Listen> {
        &mut self.ports
    }

    fn port_mapping_ref(&self) -> &HashMap<u16, u16> {
        &self.port_mapping
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_listen(
        &mut self,
        mut listen: Listen,
        tx: &Sender<ClientMessage>,
    ) -> Result<(), LayerError> {
        let original_port = listen.requested_port;
        self.apply_port_mapping(&mut listen);
        let request_port = listen.requested_port;

        if !self.ports_mut().insert(listen) {
            info!("Port {request_port} already listening, might be on different address");
            return Ok(());
        }

        let steal_type = if self.http_ports.contains(&original_port) && let Some(filter_str) = self.http_filter.take() {
            FilteredHttp(request_port, Filter::new(filter_str)?)
        } else {
            All(request_port)
        };

        tx.send(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
            steal_type,
        )))
        .await
        .map_err(From::from)
    }
}

impl TcpStealHandler {
    pub(crate) fn new(
        http_filter: Option<String>,
        http_ports: Vec<u16>,
        http_response_sender: Sender<HttpResponse>,
        port_mapping: HashMap<u16, u16>,
    ) -> Self {
        Self {
            ports: Default::default(),
            write_streams: Default::default(),
            read_streams: Default::default(),
            http_request_senders: Default::default(),
            http_response_sender,
            http_filter,
            http_ports,
            port_mapping,
        }
    }

    /// Remove a layer<->local-app connection.
    /// If the connection is still open, it will be closed, by dropping its read and write streams.
    fn remove_connection(&mut self, connection_id: ConnectionId) {
        let _ = self.read_streams.remove(&connection_id);
        let _ = self.write_streams.remove(&connection_id);
        let _ = self.http_request_senders.remove(&connection_id);
    }

    /// Remove the connection from all struct members, and return a message to notify the agent.
    fn app_closed_connection(&mut self, connection_id: ConnectionId) -> ClientMessage {
        self.remove_connection(connection_id);
        ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn next(&mut self) -> Option<ClientMessage> {
        let (connection_id, value) = self.read_streams.next().await?;
        match value {
            Some(Ok(bytes)) => Some(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                connection_id,
                bytes: bytes.to_vec(),
            }))),
            Some(Err(err)) => {
                info!("connection id {connection_id:?} read error: {err:?}");
                Some(ClientMessage::TcpSteal(
                    LayerTcpSteal::ConnectionUnsubscribe(connection_id),
                ))
            }
            None => Some(self.app_closed_connection(connection_id)),
        }
    }

    async fn create_http_connection_with_application(
        addr: SocketAddr,
    ) -> Result<SendRequest<Full<Bytes>>, HttpForwarderError> {
        let target_stream = {
            let _ = DetourGuard::new();
            TcpStream::connect(addr).await?
        };
        let (http_request_sender, connection): (SendRequest<Full<Bytes>>, _) =
            handshake(target_stream)
                .await
                .map_err::<HttpForwarderError, _>(From::from)?;

        // spawn a task to poll the connection.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("Error in http connection with addr {addr:?}: {e:?}");
            }
        });
        Ok(http_request_sender)
    }

    /// Return Ok(HttpResponse) if there is a response to send back - either the response we got
    /// from the local process, or an error response we generated.
    /// Return Err(HttpRequest) if the connection was closed too soon, and that request should be
    /// retried after reconnecting with the server.
    async fn send_http_request_to_application(
        http_request_sender: &mut SendRequest<Full<Bytes>>,
        req: HttpRequest,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<HttpResponse, HttpRequest> {
        let request_id = req.request_id;
        match http_request_sender
            .send_request(req.internal_request.clone().into())
            .await
        {
            Err(err) if err.is_closed() => {
                warn!(
                    "Sending request to local application failed with: {err:?}.
                        Seems like the local application closed the connection too early, so
                        creating a new connection and trying again."
                );
                trace!("The request to be retried: {req:?}.");
                Err(req)
            }
            Err(err) if err.is_parse() => {
                warn!("Could not parse HTTP response to filtered HTTP request, got error: {err:?}.");
                let body_message = format!("mirrord: could not parse HTTP response from local application - {err:?}");
                Ok(HttpResponse::response_from_request(
                    req,
                    StatusCode::BAD_GATEWAY,
                    &body_message,
                ))
            }
            Err(err) => {
                warn!("Request to local application failed with: {err:?}.");
                let body_message = format!("mirrord tried to forward the request to the local application and got {err:?}");
                Ok(HttpResponse::response_from_request(
                    req,
                    StatusCode::BAD_GATEWAY,
                    &body_message,
                ))
            }
            Ok(res) => Ok(
                HttpResponse::from_hyper_response(res, port, connection_id, request_id)
                    .await
                    .unwrap_or_else(|e| {
                        error!("Failed to read response to filtered http request: {e:?}. \
                        Please consider reporting this issue on \
                        https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml");
                        HttpResponse::response_from_request(
                            req,
                            StatusCode::BAD_GATEWAY,
                            "mirrord",
                        )
                    }),
            ),
        }
    }

    /// Manage a single tcp connection, forward requests, wait for responses, send responses back.
    async fn connection_task(
        addr: SocketAddr,
        mut request_receiver: Receiver<HttpRequest>,
        response_sender: Sender<HttpResponse>,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<(), HttpForwarderError> {
        let mut http_request_sender = Self::create_http_connection_with_application(addr).await?;
        // Listen for more requests in this connection and forward them to app.
        while let Some(req) = request_receiver.recv().await {
            trace!("HTTP client task received a new request to send: {req:?}.");

            let http_response = match Self::send_http_request_to_application(
                &mut http_request_sender,
                req,
                port,
                connection_id,
            )
            .await
            {
                Err(req) => {
                    // The connection was closed. Recreate connection and retry send.
                    match Self::create_http_connection_with_application(addr).await {
                        Ok(request_sender) => {
                            // Reconnecting was successful.
                            http_request_sender = request_sender;
                            Self::send_http_request_to_application(
                                &mut http_request_sender,
                                req,
                                port,
                                connection_id,
                            )
                            .await
                                .unwrap_or_else(|req| {
                                    warn!("Application closed connection too early AGAIN. Not retrying, returning error HTTP response.");
                                    HttpResponse::response_from_request(
                                        req,
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        "mirrord: local process closed the connection too early twice in a row."
                                    )
                                })
                        }
                        Err(err) => {
                            error!(
                                "The local application closed the http connection early and a \
                            new connection could not be created. Got error {err:?} when tried to \
                            reconnect."
                            );
                            HttpResponse::response_from_request(
                                req,
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "mirrord: the local process closed the connection.",
                            )
                        }
                    }
                }
                Ok(res) => res,
            };
            response_sender.send(http_response).await?;
        }
        Ok(())
    }

    /// Create a new TCP connection with the application to send all the filtered HTTP requests
    /// from this connection in.
    /// Spawn a task that receives requests on a channel and sends them to the application on that
    /// new TCP connection. The sender of that channel is stored in [`self.request_senders`].
    /// The responses from all the http client tasks will arrive together at
    /// [`self.response_receiver`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn create_http_connection(
        &mut self,
        http_request: HttpRequest,
    ) -> Result<(), LayerError> {
        let listen = self
            .ports()
            .get(&http_request.port)
            .ok_or(LayerError::PortNotFound(http_request.port))?;
        let addr: SocketAddr = listen.into();
        let connection_id = http_request.connection_id;
        let port = http_request.port;

        let (request_sender, request_receiver) = channel(1024);

        let response_sender = self.http_response_sender.clone();

        tokio::spawn(async move {
            trace!("HTTP client task started.");
            if let Err(e) =
                Self::connection_task(addr, request_receiver, response_sender, port, connection_id)
                    .await
            {
                error!(
                    "Error while forwarding http connection {connection_id} (port {port}): {e:?}."
                )
            } else {
                trace!(
                    "Filtered http connection {connection_id} (port {port}) closed without errors."
                )
            }
        });

        request_sender
            .send(http_request)
            .await
            .map_err::<HttpForwarderError, _>(From::from)?;
        // Give the forwarder a channel to send the task new requests from the same connection.
        self.http_request_senders
            .insert(connection_id, request_sender);

        trace!("main task done creating http connection.");
        Ok(())
    }
}
