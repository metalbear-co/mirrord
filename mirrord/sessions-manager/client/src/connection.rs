use std::{
    marker::{PhantomData, Send},
    time::Duration,
};

use futures::FutureExt;
use mirrord_protocol_io::{Agent, Client, Connection, ProtocolEndpoint};
use mirrord_sessions_manager_protocol::{
    ControlPlaneMessages, DataplaneReadyPayload, RegisterPayload, Role,
};
use rust_socketio::{
    self, Event as SocketIoEvent, Payload, TransportType,
    asynchronous::{Client as SocketIoClient, ClientBuilder},
};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_util::sync::CancellationToken;

use crate::websocket::BinaryWebSocketConnection;

pub const SESSIONS_MANAGER_URL_ENV: &str = "MIRRORD_SESSIONS_MANAGER_URL";
const SESSIONS_MANAGER_URL_DEFAULT: &str = "http://localhost:4971";

#[derive(thiserror::Error, Debug)]
pub enum SessionsManagerClientError {
    #[error("SocketIO control plane error: {0}")]
    SocketIO(#[from] rust_socketio::Error),

    #[error("WebSocket data plane upgrade error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("JSON serialization or deserialization failed: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("The background control plane worker dropped before providing a payload")]
    ChannelDropped,

    #[error("Control plane initialization timed out")]
    Timeout,

    #[error("Cancellation token was signaled")]
    CancellationToken,
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

/// An extension trait implemented for your data-plane types to associate
/// them with their respective wire-level control plane role string names.
pub trait ProtocolEndpointExt: ProtocolEndpoint + std::marker::Unpin + Send + 'static {
    /// The string sent to the server over Socket.IO / HTTP Query parameters
    const CONTROL_ROLE: Role;
}

impl ProtocolEndpointExt for Agent {
    const CONTROL_ROLE: Role = Role::Agent;
}

impl ProtocolEndpointExt for Client {
    const CONTROL_ROLE: Role = Role::Intproxy;
}

pub struct SessionsManagerClient<R: ProtocolEndpointExt> {
    manager_url: String,
    room_id: String,
    cancellation_token: CancellationToken,
    _marker: PhantomData<R>,
}

impl<R> SessionsManagerClient<R>
where
    R: ProtocolEndpointExt,
{
    pub fn new(
        room_id: impl Into<String>,
        cancellation_token: impl Into<Option<CancellationToken>>,
    ) -> Self {
        // If None is provided, fallback onto a pristine token that is never canceled.
        // This avoids branching in tokio::select! statements.
        let actual_token = cancellation_token
            .into()
            .unwrap_or_else(CancellationToken::new);

        let sessions_manager_url = std::env::var(SESSIONS_MANAGER_URL_ENV)
            .unwrap_or(SESSIONS_MANAGER_URL_DEFAULT.to_string());
        Self {
            manager_url: sessions_manager_url,
            room_id: room_id.into(),
            cancellation_token: actual_token,
            _marker: PhantomData,
        }
    }

    /// binds listeners, and establishes the `SocketIoClient` connection.
    async fn build_socketio_client(
        &self,
        ready_payload_tx: mpsc::UnboundedSender<DataplaneReadyPayload>,
    ) -> Result<SocketIoClient, SessionsManagerClientError> {
        let connection_room_id = self.room_id.clone();

        tracing::debug!("Sessions Manager URL: {0}", &self.manager_url);
        let client = ClientBuilder::new(&self.manager_url)
            .namespace("/")
            .transport_type(TransportType::Websocket)
            .on(SocketIoEvent::Connect, move |_, client| {
                Self::handle_control_plane_connect(connection_room_id.clone(), client).boxed()
            })
            .on(ControlPlaneMessages::Handoff, move |payload, _| {
                // Clone the tx directly into the move block closure
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
        tokio::spawn(async move {
            cancel_watcher.cancelled().await;
            tracing::info!(
                "SessionsManagerClient<{:?}> -> Cancellation triggered, tearing down control plane connection.",
                R::CONTROL_ROLE
            );
            let _ = client.disconnect().await;
        });
    }

    async fn handle_control_plane_connect(room_id: String, client: SocketIoClient) {
        tracing::debug!("Control plane active, Registering {:?}", R::CONTROL_ROLE);

        let _ = client
            .emit(
                ControlPlaneMessages::Register,
                serde_json::json!(RegisterPayload {
                    room_id,
                    role: R::CONTROL_ROLE,
                }),
            )
            .await;
    }
}

// --- GENERIC SPECIALIZATION BLOCK 1: AGENT ON MULTIPLEXED TRACK ---
impl SessionsManagerClient<Agent> {
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
    /// recieves DataplaneReadyPayload on rx and sends back Connection<Role> over the tx
    fn spawn_dataplane_allocation_worker(
        &self,
        mut ready_payload_rx: mpsc::UnboundedReceiver<DataplaneReadyPayload>,
        connection_out_tx: mpsc::UnboundedSender<Connection<Agent>>,
    ) {
        let token = self.cancellation_token.clone();
        let sessions_manager_url = self.manager_url.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Gracefully exit the loop if the token is cancelled
                    _ = token.cancelled() => {
                        tracing::debug!("SessionsManagerClient allocation worker -> Stopping on cancellation signal.");
                        break;
                    }

                    // Poll new incoming handoffs from Socket.IO callbacks
                    maybe_dataplane = ready_payload_rx.recv() => {
                        let Some(dataplane) = maybe_dataplane else {
                            tracing::warn!("Received empty dataplane notification, breaking");
                            break;
                        };
                        tracing::debug!("⚡ Control plane intercepted new handoff request: {}", dataplane.ws_path);

                        let target_ws_url = build_target_ws_url(&sessions_manager_url, &dataplane.ws_path);
                        let connection_out_tx_clone = connection_out_tx.clone();
                        let token_clone = token.clone();

                        // Connect to individual tunnels concurrently, making each sub-session cancellation aware
                        tokio::select! {
                            _ = token_clone.cancelled() => {
                                tracing::debug!("Aborting data-plane sub-session upgrade due to cancellation.");
                            }
                            connect_result = connect_async(&target_ws_url) => {
                                match connect_result {
                                    Ok((ws_stream, _)) => {
                                        tracing::debug!("🚀 Data-plane sub-session established successfully!");
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

// --- GENERIC SPECIALIZATION BLOCK 2: CLIENT ON ONE-SHOT ATOMIC TRACK ---
impl SessionsManagerClient<Client> {
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
                    if let Some(ready) = maybe_ready {
                        if let Some(tx) = dataplane_tx.take() {
                            let _ = tx.send(ready);
                        }
                    }
                }
            }
        });

        // Await synchronization barrier
        let dataplane_result = tokio::time::timeout(timeout_duration, dataplane_rx).await;

        // Immediate clean shutdown of control plane signaling
        let _ = client.disconnect().await;

        let dataplane = match dataplane_result {
            Ok(Ok(payload)) => payload,
            Ok(Err(_)) => return Err(SessionsManagerClientError::ChannelDropped),
            Err(_) => return Err(SessionsManagerClientError::Timeout),
        };

        // Utilizing URL Builder Helper
        let target_ws_url = build_target_ws_url(&self.manager_url, &dataplane.ws_path);

        tokio::select! {
            _ = self.cancellation_token.cancelled() => {
                Err(SessionsManagerClientError::CancellationToken)
            }
            connect_result = connect_async(&target_ws_url) => {
                let (ws_stream, _) = connect_result?;
                tracing::debug!("🚀 Oneshot client data-plane established successfully!");
                let binary_conn = Connection::<Client>::from_channel(
                    BinaryWebSocketConnection::<_, Client>::new(ws_stream)
                );
                Ok(binary_conn)
            }
        }
    }
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
