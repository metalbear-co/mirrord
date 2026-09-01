use std::{sync::Arc, time::Duration};

use futures::future::BoxFuture;
use mirrord_protocol_io::{Connection, ProtocolEndpoint};
use mirrord_sessions_manager_protocol::ConnectionAssignment;
use url::Url;

mod websocket;

use crate::{credentials::CredentialProvider, error::SessionsManagerClientError};

pub struct DataPlaneConnectRequest {
    pub control_plane_url: Url,
    pub assignment: ConnectionAssignment,
    /// Carried so the upgrade passes whatever fronts sessions-manager. The
    /// per-assignment authorization proves which session this is; these prove the
    /// request may reach sessions-manager at all.
    pub credentials: Arc<dyn CredentialProvider>,
}

/// Establishes a data-plane connection for either protocol endpoint.
pub trait DataPlaneTransport: Clone + Send + Sync + 'static {
    fn connect_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn connect<E>(
        &self,
        request: DataPlaneConnectRequest,
    ) -> BoxFuture<'static, Result<Connection<E>, SessionsManagerClientError>>
    where
        E: ProtocolEndpoint + Send + Unpin + 'static;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WebSocketDataPlaneTransport;

impl DataPlaneTransport for WebSocketDataPlaneTransport {
    fn connect<E>(
        &self,
        request: DataPlaneConnectRequest,
    ) -> BoxFuture<'static, Result<Connection<E>, SessionsManagerClientError>>
    where
        E: ProtocolEndpoint + Send + Unpin + 'static,
    {
        Box::pin(async move { websocket::connect_data_plane(request).await })
    }
}
