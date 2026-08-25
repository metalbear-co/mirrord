use std::{marker::PhantomData, time::Duration};

use futures::FutureExt;
use mirrord_protocol_io::{Agent, Client, Connection, ProtocolEndpoint};
use mirrord_sessions_manager_protocol::{
    ControlPlaneMessages, DataplaneReadyPayload, RegisterPayload,
};
use rust_socketio::{
    self, Event as SocketIoEvent, Payload, TransportType,
    asynchronous::{Client as SocketIoClient, ClientBuilder},
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        handshake::client::Request,
        http::{self, HeaderName, HeaderValue},
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    envs::{sessions_manager_auth_header, sessions_manager_namespace},
    error::SessionsManagerClientError,
    websocket::BinaryWebSocketConnection,
};

pub const SESSIONS_MANAGER_URL_ENV: &str = "MIRRORD_SESSIONS_MANAGER_URL";
const SESSIONS_MANAGER_URL_DEFAULT: &str = "http://localhost:4971";

/// Logical sessions-manager connection identity.
///
/// `room_id` scopes peers to the service, `namespace` optionally scopes it to an environment,
/// `session_id` keeps reconnects stable, and `target_replica_id` narrows a service-level room down
/// to one concrete workload replica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsManagerConnectInfo {
    /// Identifies the service whose agent peers should receive this connection.
    pub room_id: String,

    /// Separates services with the same identity into distinct customer environments.
    pub namespace: Option<String>,

    /// Restricts the connection to one workload replica when provided.
    pub target_replica_id: Option<String>,

    /// Keeps the intproxy identity stable across sessions-manager reconnects.
    pub session_id: String,
}

pub struct SessionsManagerClient<R: ProtocolEndpoint> {
    manager_url: String,
    /// Attached to the control-plane handshake and to every data-plane upgrade,
    /// so a sessions-manager behind an authenticating proxy is reachable
    /// without the proxy having to understand either protocol.
    auth_header: Option<(String, String)>,
    registration: RegisterPayload,
    cancellation_token: CancellationToken,
    _marker: PhantomData<R>,
}

impl<R> SessionsManagerClient<R>
where
    R: ProtocolEndpoint,
{
    fn new_with_registration(
        registration: RegisterPayload,
        cancellation_token: impl Into<Option<CancellationToken>>,
    ) -> Self {
        // If None is provided, fallback onto a pristine token that is never canceled.
        // This avoids branching in tokio::select! statements.
        let actual_token = cancellation_token
            .into()
            .unwrap_or_else(CancellationToken::new);

        let sessions_manager_url = std::env::var(SESSIONS_MANAGER_URL_ENV)
            .unwrap_or_else(|_| SESSIONS_MANAGER_URL_DEFAULT.to_owned());
        tracing::debug!(%sessions_manager_url, "Sessions Manager URL");
        let auth_header = sessions_manager_auth_header();
        if let Some((name, _)) = &auth_header {
            tracing::debug!(header = %name, "Authenticating sessions-manager connections");
        }

        Self {
            manager_url: sessions_manager_url,
            auth_header,
            registration,
            cancellation_token: actual_token,
            _marker: PhantomData,
        }
    }

    fn registration_role(&self) -> String {
        self.registration.role().to_owned()
    }

    /// binds listeners, and establishes the `SocketIoClient` connection.
    async fn build_socketio_client(
        &self,
        ready_payload_tx: mpsc::UnboundedSender<DataplaneReadyPayload>,
    ) -> Result<SocketIoClient, SessionsManagerClientError> {
        let registration = self.registration.clone();
        let sm_controlplane_url = format!("{}/control", self.manager_url.trim_end_matches('/'));

        let mut builder = ClientBuilder::new(sm_controlplane_url);
        if let Some((name, value)) = self.auth_header.clone() {
            builder = builder.opening_header(name, value);
        }

        let client = builder
            .namespace("/")
            .transport_type(TransportType::Websocket)
            .on(SocketIoEvent::Connect, move |_, client| {
                Self::handle_control_plane_connect(registration.clone(), client).boxed()
            })
            .on(ControlPlaneMessages::Handoff, move |payload, _| {
                let tx_clone = ready_payload_tx.clone();
                async move {
                    if let Ok(ready) = deserialize_payload::<DataplaneReadyPayload>(&payload) {
                        let _ = tx_clone.send(ready);
                    } else {
                        tracing::error!(
                            "Received unparseable ready frame structure from signaling channel"
                        );
                    }
                }
                .boxed()
            })
            .connect()
            .await?;

        Ok(client)
    }

    /// Spawns an automated drop-guard task that cleanly terminates Socket.IO on cancellation.
    fn spawn_cancellation_watcher(&self, client: SocketIoClient) {
        let cancel_watcher = self.cancellation_token.clone();
        let role = self.registration_role();
        tokio::spawn(async move {
            cancel_watcher.cancelled().await;
            tracing::info!(
                ?role,
                "SessionsManagerClient cancellation triggered; tearing down control plane connection"
            );
            let _ = client.disconnect().await;
        });
    }

    async fn handle_control_plane_connect(registration: RegisterPayload, client: SocketIoClient) {
        let role = registration.role().to_owned();
        tracing::debug!(role = ?role, room_id = %registration.room_id, "Control plane active, registering");

        let _ = client
            .emit(
                ControlPlaneMessages::Register,
                serde_json::to_value(registration).unwrap(),
            )
            .await;

        tracing::debug!(role = ?role, "Waiting for dataplane handoff");
    }
}

impl SessionsManagerClient<Agent> {
    pub fn new_agent(
        room_id: impl Into<String>,
        replica_id: impl Into<String>,
        cancellation_token: impl Into<Option<CancellationToken>>,
    ) -> Self {
        Self::new_with_registration(
            RegisterPayload::agent(room_id, replica_id, sessions_manager_namespace()),
            cancellation_token,
        )
    }

    /// Orchestrates a persistent control plane session over Socket.IO.
    /// Yields an MPSC receiver that emits fully configured data plane tunnel streams
    /// asynchronously.
    ///
    /// This method is strictly restricted to Agent implementations. (could be easily generalized if
    /// REALLY necessary)
    pub async fn start_multiplexed_control_plane(
        &mut self,
    ) -> Result<mpsc::UnboundedReceiver<Connection<Agent>>, SessionsManagerClientError> {
        let (ready_payload_tx, ready_payload_rx) =
            mpsc::unbounded_channel::<DataplaneReadyPayload>();
        let (connection_out_tx, connection_out_rx) = mpsc::unbounded_channel::<Connection<Agent>>();

        let client = self.build_socketio_client(ready_payload_tx).await?;
        self.spawn_cancellation_watcher(client);

        self.spawn_dataplane_allocation_worker(ready_payload_rx, connection_out_tx);

        Ok(connection_out_rx)
    }

    /// Background driver loop running concurrently to handle data plane allocation assignments.
    /// receives DataplaneReadyPayload on rx and sends back `Connection<Agent>` over the tx.
    fn spawn_dataplane_allocation_worker(
        &self,
        mut ready_payload_rx: mpsc::UnboundedReceiver<DataplaneReadyPayload>,
        connection_out_tx: mpsc::UnboundedSender<Connection<Agent>>,
    ) {
        let token = self.cancellation_token.clone();
        let sessions_manager_url = self.manager_url.clone();
        let auth_header = self.auth_header.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Gracefully exit the loop if the token is cancelled
                    _ = token.cancelled() => {
                        tracing::debug!("SessionsManagerClient allocation worker stopping on cancellation signal");
                        break;
                    }

                    // Poll new incoming handoffs from Socket.IO callbacks
                    maybe_dataplane = ready_payload_rx.recv() => {
                        let Some(dataplane) = maybe_dataplane else {
                            tracing::warn!("Received empty dataplane notification, breaking");
                            break;
                        };
                        tracing::debug!(ws_path = %dataplane.ws_path, "Control plane intercepted new handoff request");

                        let target_ws_url = build_target_ws_url(&sessions_manager_url, &dataplane.ws_path);
                        let request = match dataplane_request(&target_ws_url, auth_header.as_ref()) {
                            Ok(request) => request,
                            Err(err) => {
                                tracing::error!(error = ?err, "Failed to build data plane upgrade request");
                                continue;
                            }
                        };
                        let connection_out_tx_clone = connection_out_tx.clone();
                        let token_clone = token.clone();

                        // Connect to individual tunnels concurrently, making each sub-session cancellation aware
                        tokio::select! {
                            _ = token_clone.cancelled() => {
                                tracing::debug!("Aborting data-plane sub-session upgrade due to cancellation");
                            }
                            connect_result = connect_async(request) => {
                                match connect_result {
                                    Ok((ws_stream, _)) => {
                                        tracing::debug!("Data-plane sub-session established successfully");
                                        let binary_conn = Connection::<Agent>::from_channel(
                                            BinaryWebSocketConnection::<_, Agent>::new(ws_stream)
                                        );

                                        // Send the new stream to the runner loop execution context
                                        let _ = connection_out_tx_clone.send(binary_conn);
                                    }
                                    Err(err) => {
                                        let typed_err = SessionsManagerClientError::from(err);
                                        tracing::error!(error = ?typed_err, "Failed to upgrade multiplexed data plane session link");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

impl SessionsManagerClient<Client> {
    pub fn new_intproxy(
        connect_info: SessionsManagerConnectInfo,
        cancellation_token: impl Into<Option<CancellationToken>>,
    ) -> Self {
        Self::new_with_registration(
            RegisterPayload::intproxy(
                connect_info.room_id,
                connect_info.session_id,
                connect_info.target_replica_id,
                connect_info.namespace,
            ),
            cancellation_token,
        )
    }

    /// Establishes a single, atomic connection to the session manager.
    /// Shuts down the signaling control plane connection immediately after the first handshake
    /// settles.
    ///
    /// This method is strictly restricted to Client/Intproxy implementations.
    pub async fn connect_oneshot(
        &mut self,
        timeout_duration: Duration,
    ) -> Result<Connection<Client>, SessionsManagerClientError> {
        let (dataplane_tx, dataplane_rx) = tokio::sync::oneshot::channel::<DataplaneReadyPayload>();
        let (ready_payload_tx, mut ready_payload_rx) =
            mpsc::unbounded_channel::<DataplaneReadyPayload>();

        let client = self.build_socketio_client(ready_payload_tx).await?;

        let mut dataplane_tx = Some(dataplane_tx);
        let token = self.cancellation_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::debug!("Cancellation token signaled during wait for dataplane");
                }
                maybe_ready = ready_payload_rx.recv() => {
                    if let Some(ready) = maybe_ready && let Some(tx) = dataplane_tx.take() {
                        let _ = tx.send(ready);
                    }
                }
            }
        });

        let dataplane_result = tokio::time::timeout(timeout_duration, dataplane_rx).await;

        let _ = client.disconnect().await;

        let dataplane = match dataplane_result {
            Ok(Ok(payload)) => payload,
            Ok(Err(_)) => return Err(SessionsManagerClientError::ChannelDropped),
            Err(_) => return Err(SessionsManagerClientError::Timeout),
        };

        let target_ws_url = build_target_ws_url(&self.manager_url, &dataplane.ws_path);
        let request = dataplane_request(&target_ws_url, self.auth_header.as_ref())?;

        tokio::select! {
            _ = self.cancellation_token.cancelled() => {
                Err(SessionsManagerClientError::CancellationToken)
            }
            connect_result = connect_async(request) => {
                let (ws_stream, _) = connect_result?;
                tracing::debug!("Data-plane oneshot client established successfully");
                let binary_conn = Connection::<Client>::from_channel(
                    BinaryWebSocketConnection::<_, Client>::new(ws_stream)
                );
                Ok(binary_conn)
            }
        }
    }
}

pub fn deserialize_payload<T>(payload: &Payload) -> Result<T, serde_json::Error>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let base_value = match payload {
        Payload::Text(values) => values.first().cloned().unwrap_or(serde_json::Value::Null),
        #[allow(deprecated)]
        Payload::String(raw_str) => {
            serde_json::from_str(raw_str).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };
    let normalized_value = if base_value.is_array() {
        base_value.get(0).cloned().unwrap_or(base_value)
    } else {
        base_value
    };
    serde_json::from_value::<T>(normalized_value)
}

/// Resolves the target query string url parameters for a provided data plane path.
fn build_target_ws_url(http_url: &str, ws_path: &str) -> String {
    let base = if let Some(rest) = http_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        http_url.to_owned()
    };
    format!("{base}{ws_path}")
}

/// Builds the data-plane upgrade request, carrying the auth header when one is
/// configured.
///
/// [`connect_async`] accepts a bare URL, but that path offers nowhere to attach
/// headers, so the request is constructed explicitly.
fn dataplane_request(
    url: &str,
    auth_header: Option<&(String, String)>,
) -> Result<Request, SessionsManagerClientError> {
    let mut request = url.into_client_request()?;

    if let Some((name, value)) = auth_header {
        let name = HeaderName::try_from(name.as_str()).map_err(http::Error::from)?;
        let value = HeaderValue::try_from(value.as_str()).map_err(http::Error::from)?;
        request.headers_mut().insert(name, value);
    }

    Ok(request)
}
