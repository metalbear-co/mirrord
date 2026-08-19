//! Connects to the sessions-manager data plane and adapts its WebSocket transport to
//! [`mirrord_protocol_io::Connection`].

use std::time::Duration;

use mirrord_protocol_io::{Connection, ProtocolEndpoint, websocket};
use secrecy::ExposeSecret;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue},
};

use crate::{data_plane::DataPlaneConnectRequest, error::SessionsManagerClientError};

const WEBSOCKET_UPGRADE_TIMEOUT: Duration = Duration::from_secs(30);

/// Establishes the data-plane WebSocket described by a control-plane assignment.
///
/// Assignment endpoints are relative to the configured control-plane origin. The assignment
/// authorization is forwarded only after resolving the endpoint against that trusted origin.
pub(crate) async fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
    request: DataPlaneConnectRequest,
) -> Result<Connection<E>, SessionsManagerClientError> {
    let DataPlaneConnectRequest {
        control_plane_url: base_url,
        assignment,
    } = request;
    let scheme = match base_url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" | "wss" => base_url.scheme(),
        _ => {
            return Err(SessionsManagerClientError::InvalidBaseUrlScheme(
                base_url.clone(),
            ));
        }
    }
    .to_owned();
    let url = assignment.data_plane_endpoint.resolve(&base_url, &scheme)?;
    let mut request = url.as_str().into_client_request()?;
    let mut authorization = HeaderValue::from_str(assignment.authorization.expose_secret())
        .map_err(|_| SessionsManagerClientError::InvalidAuthorization)?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(reqwest::header::AUTHORIZATION, authorization);

    let (stream, response) =
        tokio::time::timeout(WEBSOCKET_UPGRADE_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| SessionsManagerClientError::WebSocketUpgradeTimeout)??;

    tracing::debug!(
        status = %response.status(),
        "WebSocket data-plane connection established"
    );
    Ok(websocket::connection::<_, E>(stream))
}
