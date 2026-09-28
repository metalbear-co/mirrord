//! Connects to the sessions-manager data plane and adapts its WebSocket transport to
//! [`mirrord_protocol_io::Connection`].

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{
    Request,
    header::{AUTHORIZATION, HeaderValue},
    upgrade::Upgraded,
};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{
    client::legacy::Client as HyperClient,
    rt::{TokioExecutor, TokioIo},
};
use mirrord_operator_websocket::{
    connection::OperatorConnection,
    upgrade::{BoxError, UpgradeContract, connect_ws_direct},
};
use mirrord_protocol_io::ProtocolEndpoint;
use secrecy::ExposeSecret;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;

use crate::{data_plane::DataPlaneConnectRequest, error::SessionsManagerClientError};

const WEBSOCKET_UPGRADE_TIMEOUT: Duration = Duration::from_secs(30);

/// Establishes the data-plane WebSocket described by a control-plane assignment.
///
/// Assignment endpoints are relative to the configured control-plane origin. The assignment
/// authorization is forwarded only after resolving the endpoint against that trusted origin.
///
/// Incoming binary messages are decoded according to `E`. Outgoing pre-encoded payloads are sent
/// as individual binary WebSocket messages.
pub(crate) async fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
    request: DataPlaneConnectRequest,
) -> Result<OperatorConnection<E>, SessionsManagerClientError> {
    Ok(OperatorConnection::new(connect_websocket(request).await?))
}

/// Dials and authorizes the data-plane WebSocket described by a control-plane assignment.
async fn connect_websocket(
    request: DataPlaneConnectRequest,
) -> Result<WebSocketStream<TokioIo<Upgraded>>, SessionsManagerClientError> {
    let DataPlaneConnectRequest {
        control_plane_url: base_url,
        assignment,
        credentials,
    } = request;

    let scheme = match base_url.scheme() {
        "http" | "ws" => "http",
        "https" | "wss" => "https",
        _ => {
            return Err(SessionsManagerClientError::InvalidBaseUrlScheme(
                base_url.clone(),
            ));
        }
    };

    let url = assignment.data_plane_endpoint.resolve(&base_url, scheme)?;
    let mut request = Request::builder().uri(url.as_str()).body(Vec::new())?;
    request.headers_mut().extend(credentials.headers()?);

    let mut authorization = HeaderValue::from_str(assignment.authorization.expose_secret())
        .map_err(|_| SessionsManagerClientError::InvalidAuthorization)?;
    authorization.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, authorization);

    let connector = HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    let client: HyperClient<_, Empty<Bytes>> =
        HyperClient::builder(TokioExecutor::new()).build(connector);

    let started_at = Instant::now();
    tokio::time::timeout(WEBSOCKET_UPGRADE_TIMEOUT, async {
        // The data-plane upgrade request never carries a body, so the (always-empty) `Vec<u8>`
        // built above is simply dropped in favor of the client's own `Empty<Bytes>` body type.
        let stream = connect_ws_direct(request, UpgradeContract::Direct, |request| async {
            let response = client
                .request(request.map(|_| Empty::<Bytes>::new()))
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            Ok(response.map(|body| {
                body.map_err(|error| Box::new(error) as BoxError)
                    .boxed_unsync()
            }))
        })
        .await?;

        let elapsed = started_at.elapsed();
        tracing::debug!(
            ?url,
            ?elapsed,
            "WebSocket data-plane connection established"
        );

        Ok::<_, SessionsManagerClientError>(stream)
    })
    .await
    .map_err(|_| SessionsManagerClientError::WebSocketUpgradeTimeout)?
}
