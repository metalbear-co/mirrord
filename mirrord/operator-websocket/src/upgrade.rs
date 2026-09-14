//! Code copied from [`kube::client`] and adjusted.
//!
//! Just like original [`Client::connect`] function, [`connect_ws`] creates a
//! WebSockets connection. However, original function swallows
//! [`Status`] sent by the operator and returns flat
//! [`UpgradeConnectionError`]. [`connect_ws`] attempts to
//! recover the [`Status`] - if operator response code is not
//! [`StatusCode::SWITCHING_PROTOCOLS`], it tries to read
//! response body and deserialize it.
//!
//! [`connect_ws_direct`] performs the same handshake through a caller-supplied Hyper client
//! instead of a [`kube::Client`], targeting either the API-server or a plain HTTPS endpoint
//! depending on the given [`UpgradeContract`]. [`connect_ws`] is a thin wrapper around it for the
//! [`kube::Client`] case.

use std::future::Future;

use base64::Engine;
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::{BodyExt, combinators::UnsyncBoxBody};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use kube::{
    Client, Error,
    client::{Body, UpgradeConnectionError},
    core::Status,
};
use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

const WS_PROTOCOL: &str = "v4.channel.k8s.io";

/// Type-erased error shared by [`connect_ws`] and [`connect_ws_direct`] for transport and
/// body-read failures.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Body type shared by [`connect_ws`] and [`connect_ws_direct`], unifying the different concrete
/// body types their two send paths produce.
pub type ResponseBody = UnsyncBoxBody<Bytes, BoxError>;

/// Failures [`verify_response`] and [`finish_ws_upgrade`] can produce.
///
/// Kept separate from [`ConnectError`] so [`connect_ws`] can map it onto [`kube::Error`] with an
/// exhaustive match, since request-sending errors are handled earlier and never reach this type.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeFailure {
    /// The operator returned a [`Status`] object instead of switching protocols.
    #[error("{0}")]
    Api(Box<Status>),
    /// The WebSocket handshake failed, e.g. a header was missing or didn't match.
    #[error("websocket upgrade failed: {0}")]
    Upgrade(#[from] UpgradeConnectionError),
}

/// Errors from [`connect_ws_direct`]. [`connect_ws`] surfaces the same failures as [`kube::Error`]
/// instead, to match its existing callers.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The WebSocket handshake failed; see [`UpgradeFailure`].
    #[error(transparent)]
    Upgrade(#[from] UpgradeFailure),
    /// Sending the request failed, e.g. a connection or TLS error.
    #[error("failed to send the upgrade request: {0}")]
    Transport(#[source] BoxError),
}

/// Selects which upgrade-response contract [`connect_ws`] or [`connect_ws_direct`] must satisfy.
///
/// [`connect_ws`] always uses [`Self::KubeApiserver`]. [`connect_ws_direct`] takes it as a
/// parameter so a caller without a [`kube::Client`] can still opt into the API-server's contract.
pub enum UpgradeContract {
    /// Talking to the Kubernetes API-server's proxy subresource: requires the `v4.channel.k8s.io`
    /// subprotocol and returns a [`Status`] JSON object in non-101 response bodies.
    KubeApiserver,
    /// A plain RFC 6455 upgrade with no subprotocol negotiation.
    Direct,
}

// Verify upgrade response according to RFC6455.
// Based on `tungstenite` and added subprotocol verification.
async fn verify_response(
    res: Response<ResponseBody>,
    key: &HeaderValue,
    contract: &UpgradeContract,
) -> Result<Response<ResponseBody>, UpgradeFailure> {
    let status = res.status();

    if status != StatusCode::SWITCHING_PROTOCOLS {
        if matches!(contract, UpgradeContract::KubeApiserver)
            && (status.is_client_error() || status.is_server_error())
        {
            let error_response = res
                .into_body()
                .collect()
                .await
                .ok()
                .map(|body| body.to_bytes())
                .and_then(|body_bytes| serde_json::from_slice::<Status>(&body_bytes).ok());

            if let Some(error_response) = error_response {
                return Err(UpgradeFailure::Api(Box::new(error_response)));
            }
        }

        return Err(UpgradeConnectionError::ProtocolSwitch(status).into());
    }

    let headers = res.headers();
    if !headers
        .get(http::header::UPGRADE)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
    {
        return Err(UpgradeConnectionError::MissingUpgradeWebSocketHeader.into());
    }

    if !headers
        .get(http::header::CONNECTION)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.eq_ignore_ascii_case("Upgrade"))
        .unwrap_or(false)
    {
        return Err(UpgradeConnectionError::MissingConnectionUpgradeHeader.into());
    }

    let accept_key = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_ref());
    if !headers
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .map(|h| h == &accept_key)
        .unwrap_or(false)
    {
        return Err(UpgradeConnectionError::SecWebSocketAcceptKeyMismatch.into());
    }

    // Make sure that the server returned the correct subprotocol.
    if matches!(contract, UpgradeContract::KubeApiserver)
        && !headers
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .map(|h| h == WS_PROTOCOL)
            .unwrap_or(false)
    {
        return Err(UpgradeConnectionError::SecWebSocketProtocolMismatch.into());
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

/// Attaches the RFC 6455 handshake headers to `request`, adding the `v4.channel.k8s.io`
/// subprotocol for [`UpgradeContract::KubeApiserver`]. Also returns the `Sec-WebSocket-Key` used
/// to verify the response.
fn prepare_request(
    request: Request<Vec<u8>>,
    contract: &UpgradeContract,
) -> (Request<Vec<u8>>, HeaderValue) {
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

    if matches!(contract, UpgradeContract::KubeApiserver) {
        // Use the binary subprotocol v4, to get JSON `Status` object in `error` channel (3).
        // There's no official documentation about this protocol, but it's described in
        // [`k8s.io/apiserver/pkg/util/wsstream/conn.go`](https://git.io/JLQED).
        // There's a comment about v4 and `Status` object in
        // [`kublet/cri/streaming/remotecommand/httpstream.go`](https://git.io/JLQEh).
        parts.headers.insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(WS_PROTOCOL),
        );
    }

    (Request::from_parts(parts, body), key)
}

/// Verifies `response` and, if it switched protocols, turns it into a [`WebSocketStream`].
async fn finish_ws_upgrade(
    response: Response<ResponseBody>,
    key: &HeaderValue,
    contract: &UpgradeContract,
) -> Result<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>, UpgradeFailure> {
    let verified_response = verify_response(response, key, contract).await?;
    match hyper::upgrade::on(verified_response).await {
        Ok(upgraded) => {
            Ok(WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await)
        }
        Err(error) => Err(UpgradeConnectionError::GetPendingUpgrade(error).into()),
    }
}

/// Connects a WebSocket to the operator through the Kubernetes API-server, using `client`.
pub async fn connect_ws(
    client: &Client,
    request: Request<Vec<u8>>,
) -> kube::Result<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
    connect_ws_direct(request, UpgradeContract::KubeApiserver, |request| async {
        let response = client
            .send(request.map(Body::from))
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(response.map(|body| {
            body.map_err(|error| Box::new(error) as BoxError)
                .boxed_unsync()
        }))
    })
    .await
    .map_err(|error| match error {
        ConnectError::Upgrade(UpgradeFailure::Api(status)) => Error::Api(status),
        ConnectError::Upgrade(UpgradeFailure::Upgrade(error)) => Error::UpgradeConnection(error),
        // `Client::send`'s own errors are boxed into `Transport` above, so this arm only ever
        // sees those - `Error::Service` is kube's own catch-all for a boxed transport error.
        ConnectError::Transport(error) => Error::Service(error),
    })
}

/// Connects a WebSocket through a caller-supplied Hyper client instead of a [`kube::Client`].
///
/// `send` performs the HTTP request; this function only adds the RFC 6455 handshake headers (per
/// `contract`) and turns the upgraded response into a [`WebSocketStream`].
pub async fn connect_ws_direct<F, Fut>(
    request: Request<Vec<u8>>,
    contract: UpgradeContract,
    send: F,
) -> Result<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>, ConnectError>
where
    F: FnOnce(Request<Vec<u8>>) -> Fut,
    Fut: Future<Output = Result<Response<ResponseBody>, BoxError>>,
{
    let (request, key) = prepare_request(request, &contract);
    let response = send(request).await.map_err(ConnectError::Transport)?;
    Ok(finish_ws_upgrade(response, &key, &contract).await?)
}
