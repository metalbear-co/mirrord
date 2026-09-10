//! Connects to the sessions-manager data plane and adapts its WebSocket transport to
//! [`mirrord_protocol_io::Connection`].

use std::time::Duration;

use mirrord_protocol_io::{
    Connection, ProtocolEndpoint,
    websocket::{self, WebSocketChannel},
};
use secrecy::ExposeSecret;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
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
    Ok(websocket::connection::<_, E>(
        connect_websocket(request).await?,
    ))
}

/// Same as [`connect_data_plane`], but returns the raw [`WebSocketChannel`] instead of wrapping it
/// in a [`Connection`].
///
/// [`WebSocketChannel`] already implements [`futures::Sink`] and [`futures::Stream`] directly over
/// [`mirrord_protocol`](https://docs.rs/mirrord-protocol) messages, so callers that want to drive
/// the connection themselves (e.g. `mirrord-protocol-api`'s `MirrordClient`) can use it without
/// going through the [`Connection`] channel abstraction.
pub(crate) async fn connect_data_plane_raw<E: ProtocolEndpoint + Send + Unpin + 'static>(
    request: DataPlaneConnectRequest,
) -> Result<WebSocketChannel<MaybeTlsStream<TcpStream>, E>, SessionsManagerClientError> {
    Ok(WebSocketChannel::new(connect_websocket(request).await?))
}

/// Dials and authorizes the data-plane WebSocket described by a control-plane assignment.
async fn connect_websocket(
    request: DataPlaneConnectRequest,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, SessionsManagerClientError> {
    let DataPlaneConnectRequest {
        control_plane_url: base_url,
        assignment,
        credentials,
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

    // The credentials are gathered inside the timeout: a provider may have to reach the network
    // for them, and that wait belongs to the upgrade it is holding up.
    let upgrade = async move {
        let mut request = url.as_str().into_client_request()?;
        request
            .headers_mut()
            .extend(credentials.data_plane_headers().await?);
        let mut authorization = HeaderValue::from_str(assignment.authorization.expose_secret())
            .map_err(|_| SessionsManagerClientError::InvalidAuthorization)?;
        authorization.set_sensitive(true);
        request
            .headers_mut()
            .insert(reqwest::header::AUTHORIZATION, authorization);

        connect_async(request)
            .await
            .map_err(SessionsManagerClientError::from)
    };
    let (stream, response) = tokio::time::timeout(WEBSOCKET_UPGRADE_TIMEOUT, upgrade)
        .await
        .map_err(|_| SessionsManagerClientError::WebSocketUpgradeTimeout)??;

    tracing::debug!(
        status = %response.status(),
        "WebSocket data-plane connection established"
    );
    Ok(stream)
}
