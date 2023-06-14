use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use bimap::BiMap;
use futures::TryFutureExt;
use hyper::{body::Incoming, Response, StatusCode};
use mirrord_protocol::{
    tcp::{
        Filter, HttpFilter, HttpRequest, HttpResponse, LayerTcpSteal, NewTcpConnection,
        StealType::{All, FilteredHttp, FilteredHttpEx},
        TcpClose, TcpData,
    },
    ClientMessage, ConnectionId, Port, RequestId,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc::{channel, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::{error, info, trace, warn};

use crate::{
    error::LayerError,
    tcp::{Listen, TcpHandler},
    tcp_steal::http::{v1::HttpV1, v2::HttpV2, ConnectionTask},
};

pub(crate) mod http_forwarding;

use self::http::{HttpFilterSettings, LayerHttpFilter};
use crate::tcp_steal::http_forwarding::HttpForwarderError;

mod http;

#[tracing::instrument(level = "trace")]
async fn handle_response(
    request: HttpRequest,
    response: Result<Response<Incoming>, hyper::Error>,
    port: Port,
    connection_id: ConnectionId,
    request_id: RequestId,
) -> Result<HttpResponse, HttpForwarderError> {
    match response {
            Err(err) if err.is_closed() => {
                warn!(
                    "Sending request to local application failed with: {err:?}.
                        Seems like the local application closed the connection too early, so
                        creating a new connection and trying again."
                );
                trace!("The request to be retried: {request:?}.");
                Err(HttpForwarderError::ConnectionClosedTooSoon(request))
            }
            Err(err) if err.is_parse() => {
                warn!("Could not parse HTTP response to filtered HTTP request, got error: {err:?}.");
                let body_message = format!("mirrord: could not parse HTTP response from local application - {err:?}");
                Ok(HttpResponse::response_from_request(
                    request,
                    StatusCode::BAD_GATEWAY,
                    &body_message,
                ))
            }
            Err(err) => {
                warn!("Request to local application failed with: {err:?}.");
                let body_message = format!("mirrord tried to forward the request to the local application and got {err:?}");
                Ok(HttpResponse::response_from_request(
                    request,
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
                            request,
                            StatusCode::BAD_GATEWAY,
                            "mirrord",
                        )
                    }),
            ),
        }
}

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

    /// HTTP filter settings
    http_filter_settings: HttpFilterSettings,

    /// LocalPort:RemotePort mapping.
    port_mapping: BiMap<u16, u16>,
}

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
        connection.write_all(&data.bytes[..]).await.map_err(|err| {
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

    fn port_mapping_ref(&self) -> &BiMap<u16, u16> {
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

        if self.ports_mut().replace(listen).is_some() {
            // This can also be because we currently don't inform the tcp handler when an app closes
            // a socket (stops listening).
            info!("Received listen hook message for port {request_port} while already listening. Might be on different address",);
            return Ok(());
        }

        let steal_type = if self.http_filter_settings.ports.contains(&original_port) {
            match self.http_filter_settings.filter {
                LayerHttpFilter::None => All(request_port),
                LayerHttpFilter::HeaderDeprecated(ref header) => {
                    FilteredHttp(request_port, Filter::new(header.clone())?)
                }
                LayerHttpFilter::Header(ref header) => FilteredHttpEx(
                    request_port,
                    HttpFilter::Header(Filter::new(header.clone())?),
                ),
                LayerHttpFilter::Path(ref path) => {
                    FilteredHttpEx(request_port, HttpFilter::Path(Filter::new(path.clone())?))
                }
            }
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
        http_response_sender: Sender<HttpResponse>,
        port_mapping: BiMap<u16, u16>,
        http_filter_settings: HttpFilterSettings,
    ) -> Self {
        Self {
            ports: Default::default(),
            write_streams: Default::default(),
            read_streams: Default::default(),
            http_request_senders: Default::default(),
            http_response_sender,
            http_filter_settings,
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

        let http_version = http_request.version();

        tokio::spawn(async move {
            trace!("HTTP/{http_version:?} client task started.");
            let connection_task_result = match http_version {
                hyper::Version::HTTP_2 => {
                    ConnectionTask::<HttpV2>::new(
                        addr,
                        request_receiver,
                        response_sender,
                        port,
                        connection_id,
                    )
                    .and_then(ConnectionTask::start)
                    .await
                }
                hyper::Version::HTTP_3 => {
                    error!("mirrord (currently) does not support HTTP/3!");
                    todo!()
                }
                _http_v1 => {
                    ConnectionTask::<HttpV1>::new(
                        addr,
                        request_receiver,
                        response_sender,
                        port,
                        connection_id,
                    )
                    .and_then(ConnectionTask::start)
                    .await
                }
            };

            if let Err(e) = connection_task_result {
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
