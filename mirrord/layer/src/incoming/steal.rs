use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use bimap::BiMap;
use mirrord_protocol::{
    tcp::{
        Filter, HttpFilter, HttpRequestFallback, HttpResponseFallback,
        LayerTcpSteal,
        StealType::{All, FilteredHttp, FilteredHttpEx},
        TcpData,
    },
    ClientMessage, ConnectionId, Port,
};
use streammap_ext::StreamMap;
use tracing::{error, info, trace};

use crate::{
    error::LayerError,
    incoming::{Listen, TcpHandler},
};

pub(crate) mod http_forwarding;

use self::http::{HttpFilterSettings, LayerHttpFilter};
use crate::tcp_steal::http_forwarding::HttpForwarderError;

mod http;

pub struct TcpStealHandler {
    ports: HashSet<Listen>,
    write_streams: HashMap<ConnectionId, WriteHalf<TcpStream>>,
    read_streams: StreamMap<ConnectionId, ReaderStream<ReadHalf<TcpStream>>>,

    /// Mapping of a ConnectionId to a sender that sends HTTP requests over to a task that is
    /// running an http client for this connection.
    http_request_senders: HashMap<ConnectionId, Sender<HttpRequestFallback>>,

    /// Sender of responses from within an http client task back to the main layer task.
    /// This sender is cloned and moved into those tasks.
    http_response_sender: Sender<HttpResponseFallback>,

    /// HTTP filter settings
    http_filter_settings: HttpFilterSettings,

    /// LocalPort:RemotePort mapping.
    port_mapping: BiMap<u16, u16>,
}

impl TcpHandler for TcpStealHandler {
    fn handle_listen(
        &mut self,
        mut listen: Listen,
    ) -> Result<(), LayerError> {
        let original_port = listen.requested_port;
        self.apply_port_mapping(&mut listen);
        let request_port = listen.requested_port;

        if self.ports_mut().replace(listen).is_some() {
            // Since this struct identifies listening sockets by their port (#1558), we can only
            // forward the incoming traffic to one socket with that port.
            info!(
                "Received listen hook message for port {request_port} while already listening. \
                Might be on different address. Sending all incoming traffic to newest socket."
            );
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

    fn handle_close(
        &mut self,
        port: Port,
    ) -> Result<(), LayerError> {
        if self.ports_mut().remove(&port) {
            // There is a known issue - https://github.com/metalbear-co/mirrord/issues/1575 - where
            // the agent is currently not able to keep ongoing connections, so once we send this
            // message, any ongoing connections are going to be interrupted.
            tx.send(ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(
                port,
            )))
            .await
            .map_err(From::from)
        } else {
            // was not listening on closed socket, could be an outgoing socket.
            Ok(())
        }
    }
}

impl TcpStealHandler {
    pub(crate) fn new(
        http_response_sender: Sender<HttpResponseFallback>,
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
        http_request: HttpRequestFallback,
    ) -> Result<(), LayerError> {
        let port = http_request.port();
        let connection_id = http_request.connection_id();

        let listen = self
            .ports()
            .get(&port)
            .ok_or(LayerError::NewConnectionAfterSocketClose(connection_id))?;
        let addr: SocketAddr = listen.into();

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
