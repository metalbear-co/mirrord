//! Code copied from [`kube::client`] and adjusted.
//!
//! Just like original [`Client::connect`] function, [`connect_ws`] creates a
//! WebSockets connection. However, original function swallows
//! [`ErrorResponse`] sent by the operator and returns flat
//! [`UpgradeConnectionError`]. [`connect_ws`] attempts to
//! recover the [`ErrorResponse`] - if operator response code is not
//! [`StatusCode::SWITCHING_PROTOCOLS`], it tries to read
//! response body and deserialize it.

use base64::Engine;
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use kube::{
    Client, Error, Result,
    client::{Body, UpgradeConnectionError},
    core::ErrorResponse,
};
use mirrord_kube::BearClient;
use tokio_retry::{
    Retry, RetryIf,
    strategy::{ExponentialBackoff, jitter},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

const WS_PROTOCOL: &str = "v4.channel.k8s.io";

// Verify upgrade response according to RFC6455.
// Based on `tungstenite` and added subprotocol verification.
async fn verify_response(res: Response<Body>, key: &HeaderValue) -> Result<Response<Body>> {
    let status = res.status();

    if status != StatusCode::SWITCHING_PROTOCOLS {
        if status.is_client_error() || status.is_server_error() {
            let error_response = res
                .into_body()
                .collect()
                .await
                .ok()
                .map(|body| body.to_bytes())
                .and_then(|body_bytes| serde_json::from_slice::<ErrorResponse>(&body_bytes).ok());

            if let Some(error_response) = error_response {
                return Err(Error::Api(error_response));
            }
        }

        return Err(Error::UpgradeConnection(
            UpgradeConnectionError::ProtocolSwitch(status),
        ));
    }

    let headers = res.headers();
    if !headers
        .get(http::header::UPGRADE)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
    {
        return Err(Error::UpgradeConnection(
            UpgradeConnectionError::MissingUpgradeWebSocketHeader,
        ));
    }

    if !headers
        .get(http::header::CONNECTION)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.eq_ignore_ascii_case("Upgrade"))
        .unwrap_or(false)
    {
        return Err(Error::UpgradeConnection(
            UpgradeConnectionError::MissingConnectionUpgradeHeader,
        ));
    }

    let accept_key = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_ref());
    if !headers
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .map(|h| h == &accept_key)
        .unwrap_or(false)
    {
        return Err(Error::UpgradeConnection(
            UpgradeConnectionError::SecWebSocketAcceptKeyMismatch,
        ));
    }

    // Make sure that the server returned the correct subprotocol.
    if !headers
        .get(http::header::SEC_WEBSOCKET_PROTOCOL)
        .map(|h| h == WS_PROTOCOL)
        .unwrap_or(false)
    {
        return Err(Error::UpgradeConnection(
            UpgradeConnectionError::SecWebSocketProtocolMismatch,
        ));
    }

    Ok(res)
}

/// Generate a random key for the `Sec-WebSocket-Key` header.
/// This must be nonce consisting of a randomly selected 16-byte value in base64.
fn sec_websocket_key() -> HeaderValue {
    let random: [u8; 16] = rand::random();
    base64::engine::general_purpose::STANDARD
        .encode(random)
        .parse()
        .expect("should be valid")
}

pub async fn connect_ws(
    client: &Client,
    request: Request<Vec<u8>>,
) -> kube::Result<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
    let (mut parts, body) = request.into_parts();
    parts.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("Upgrade"),
    );
    parts
        .headers
        .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
    parts.headers.insert(
        http::header::SEC_WEBSOCKET_VERSION,
        HeaderValue::from_static("13"),
    );
    let key = sec_websocket_key();
    parts
        .headers
        .insert(http::header::SEC_WEBSOCKET_KEY, key.clone());
    // Use the binary subprotocol v4, to get JSON `Status` object in `error` channel (3).
    // There's no official documentation about this protocol, but it's described in
    // [`k8s.io/apiserver/pkg/util/wsstream/conn.go`](https://git.io/JLQED).
    // There's a comment about v4 and `Status` object in
    // [`kublet/cri/streaming/remotecommand/httpstream.go`](https://git.io/JLQEh).
    parts.headers.insert(
        http::header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(WS_PROTOCOL),
    );

    let retry_strategy = ExponentialBackoff::from_millis(10).map(jitter).take(1);

    let response = Retry::spawn(retry_strategy, || async {
        let request = Request::from_parts(parts.clone(), Body::from(body.clone()));
        client.send(request).await
    })
    .await?;

    let verified_response = verify_response(response, &key).await?;
    match hyper::upgrade::on(verified_response).await {
        Ok(upgraded) => {
            Ok(WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await)
        }

        Err(e) => Err(Error::UpgradeConnection(
            UpgradeConnectionError::GetPendingUpgrade(e),
        )),
    }
}
