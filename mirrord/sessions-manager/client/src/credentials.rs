//! Credentials sent with sessions-manager requests.
//!
//! Two independent mechanisms live here, and a deployment may use either, both, or neither:
//!
//! - [`SharedSecretCredentials`] proves a request may reach sessions-manager at all, to an
//!   authenticating proxy or load balancer placed in front of it.
//! - [`CloudTokenCredentials`] proves *who* is calling to sessions-manager itself, by exchanging a
//!   long-lived MetalBear API key for a short-lived token.
//!
//! Neither is the per-assignment authorization the control plane hands out; that one is minted
//! per data-plane connection and travels with the assignment.

use std::{
    env::VarError,
    sync::Arc,
    time::{Duration, SystemTime},
};

use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use futures::future::{BoxFuture, FutureExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use crate::error::SessionsManagerClientError;

/// Supplies the headers that authenticate a client's sessions-manager requests.
///
/// The two planes are asked separately because they do not authenticate against the same thing.
/// The control plane is a plain HTTP/SSE API that has to establish the caller's identity from
/// scratch on every request; the data-plane upgrade instead presents the single-use credential
/// the control plane just minted for that one assignment, under `authorization`. A credential
/// that belongs to only one of the two would otherwise have to be filtered out by the transport
/// that must not send it, which is exactly the kind of invisible coupling
/// [`RESERVED_HEADER_NAMES`] exists to catch.
///
/// Producing the headers is asynchronous because a provider may have to reach the network for
/// them (see [`CloudTokenCredentials`]). Both callers are already inside a cancellable,
/// deadline-bounded async path, so a slow or failing provider fails the attempt that needed it
/// and is retried with it, rather than being handled out of band.
pub trait CredentialProvider: Send + Sync {
    fn control_plane_headers(&self)
    -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>>;

    /// Defaults to [`CredentialProvider::control_plane_headers`], which is right for anything
    /// that authenticates the client to a fronting proxy: the proxy sees both planes and has to
    /// be satisfied by both.
    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        self.control_plane_headers()
    }
}

fn ready_headers(
    headers: HeaderMap,
) -> BoxFuture<'static, Result<HeaderMap, SessionsManagerClientError>> {
    std::future::ready(Ok(headers)).boxed()
}

#[derive(Default)]
pub(crate) struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(HeaderMap::new())
    }
}

/// Shared secret expected by whatever fronts sessions-manager. Setting it is what
/// turns [`SharedSecretCredentials`] on.
pub const SESSIONS_MANAGER_AUTH_TOKEN_ENV: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN";

/// Names the header carrying [`SESSIONS_MANAGER_AUTH_TOKEN_ENV`]. Only needed when the
/// deployment expects something other than [`DEFAULT_AUTH_HEADER_NAME`].
pub const SESSIONS_MANAGER_AUTH_HEADER_ENV: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_HEADER";

const DEFAULT_AUTH_HEADER_NAME: &str = "x-mirrord-sm-auth";

/// Header names the client sets for itself, which the shared secret may not take over.
///
/// Every one of these is applied after the credential headers, so a shared secret sent under
/// one of these names is either replaced or duplicated rather than delivered: `authorization`
/// carries the per-assignment token on the data plane, `accept` selects the control-plane
/// event stream, and the rest are part of the WebSocket handshake. The failure would other-
/// wise be near-invisible — the control plane authenticating while every data-plane upgrade
/// is rejected by the fronting proxy — so a name from this list is refused up front.
const RESERVED_HEADER_NAMES: &[&str] = &[
    "authorization",
    "accept",
    "host",
    "connection",
    "upgrade",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
];

/// Sends a fixed shared secret on every sessions-manager request, for deployments that put
/// an authenticating proxy or load balancer in front of it.
///
/// This is distinct from the per-assignment authorization the control plane hands out: the
/// proxy decides whether a request reaches sessions-manager at all, and has to make that
/// call without understanding the control-plane or data-plane protocol.
pub struct SharedSecretCredentials {
    name: HeaderName,
    value: HeaderValue,
}

impl SharedSecretCredentials {
    /// Reads the shared secret from the environment.
    ///
    /// [`None`] when no token is set, which is the case for a directly reachable
    /// sessions-manager.
    pub fn from_env() -> Result<Option<Self>, SessionsManagerClientError> {
        let token = match std::env::var(SESSIONS_MANAGER_AUTH_TOKEN_ENV) {
            Ok(token) => token,
            Err(VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let name = std::env::var(SESSIONS_MANAGER_AUTH_HEADER_ENV)
            .unwrap_or_else(|_| DEFAULT_AUTH_HEADER_NAME.to_owned());

        Self::new(&name, &token).map(Some)
    }

    pub fn new(name: &str, token: &str) -> Result<Self, SessionsManagerClientError> {
        let name = HeaderName::try_from(name).map_err(|_| {
            SessionsManagerClientError::InvalidConfig(format!("invalid auth header name: {name}"))
        })?;
        // `HeaderName` parsing lowercases, so this comparison needs no normalizing of its own.
        if RESERVED_HEADER_NAMES.contains(&name.as_str()) {
            return Err(SessionsManagerClientError::InvalidConfig(format!(
                "auth header name {name} is reserved by the sessions-manager client"
            )));
        }
        let mut value = HeaderValue::from_str(token).map_err(|_| {
            SessionsManagerClientError::InvalidConfig(
                "auth token is not a valid header value".to_owned(),
            )
        })?;
        value.set_sensitive(true);

        Ok(Self { name, value })
    }

    fn header_map(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(self.name.clone(), self.value.clone());
        headers
    }
}

impl CredentialProvider for SharedSecretCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(self.header_map())
    }
}

/// Long-lived MetalBear API key, exchanged for the short-lived token sessions-manager verifies.
/// Setting it is what turns [`CloudTokenCredentials`] on.
pub const SESSIONS_MANAGER_API_KEY_ENV: &str = "MIRRORD_SESSIONS_MANAGER_API_KEY";

/// Origin of the MetalBear cloud that mints tokens, for pointing a client at somewhere other
/// than [`METALBEAR_CLOUD_URL_DEFAULT`] (staging, or a mock in tests).
pub const METALBEAR_CLOUD_URL_ENV: &str = "MIRRORD_METALBEAR_CLOUD_URL";

const METALBEAR_CLOUD_URL_DEFAULT: &str = "https://app.metalbear.com";

/// Every MetalBear API key carries this prefix. Checking it up front turns "the key in your
/// environment is not a key at all" into a config error at startup instead of a 401 later.
const API_KEY_PREFIX: &str = "metalbear_key_";

const TOKEN_EXCHANGE_SEGMENTS: [&str; 3] = ["api", "v2", "token"];

/// How long before a token's `exp` it is replaced. Covers the request the token is about to be
/// used for, plus clock skew between this process and whatever verifies the signature.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);

const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize)]
struct TokenExchangeRequest<'a> {
    #[serde(rename = "apiKey")]
    api_key: &'a str,
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    token: String,
}

/// The one claim this client reads. The rest (`sub`, `scope`, ...) are for the server that
/// verifies the token.
#[derive(Deserialize)]
struct TokenClaims {
    exp: u64,
}

struct CachedToken {
    /// Complete `Bearer <jwt>` value, marked sensitive.
    header: HeaderValue,
    /// When the token stops being handed out, already reduced by [`TOKEN_REFRESH_MARGIN`], so
    /// deciding whether to reuse it is a single comparison against the clock.
    refresh_at: SystemTime,
}

/// Authenticates the client to sessions-manager itself, by trading a long-lived MetalBear API
/// key for a short-lived token and sending it as `Authorization: Bearer <token>`.
///
/// The exchange endpoint is unauthenticated apart from the key in the request body, so the key
/// must never leave this process by any other route — neither it nor the tokens it yields are
/// ever logged.
///
/// A token is fetched on first use and reused until [`TOKEN_REFRESH_MARGIN`] before its `exp`.
/// The mutex is held across the exchange so that concurrent callers waiting on a cold or stale
/// cache collapse into a single request rather than each starting one of their own.
///
/// Control plane only: the data-plane upgrade already sends the single-use per-assignment
/// credential under `authorization`, and must not have it replaced.
pub struct CloudTokenCredentials {
    client: reqwest::Client,
    endpoint: Url,
    api_key: SecretString,
    cached: Mutex<Option<CachedToken>>,
}

impl CloudTokenCredentials {
    /// Reads the API key, and optionally the cloud origin, from the environment.
    ///
    /// [`None`] when no key is set, which is the case for a sessions-manager that does not
    /// authenticate its callers.
    pub fn from_env() -> Result<Option<Self>, SessionsManagerClientError> {
        let api_key = match std::env::var(SESSIONS_MANAGER_API_KEY_ENV) {
            Ok(api_key) => api_key,
            Err(VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let cloud_url = std::env::var(METALBEAR_CLOUD_URL_ENV)
            .unwrap_or_else(|_| METALBEAR_CLOUD_URL_DEFAULT.to_owned());

        Self::new(&cloud_url, api_key).map(Some)
    }

    pub fn new(cloud_url: &str, api_key: String) -> Result<Self, SessionsManagerClientError> {
        if !api_key.starts_with(API_KEY_PREFIX) {
            return Err(SessionsManagerClientError::InvalidConfig(format!(
                "{SESSIONS_MANAGER_API_KEY_ENV} must hold a MetalBear API key, \
                 which begins with `{API_KEY_PREFIX}`"
            )));
        }

        let mut endpoint = Url::parse(cloud_url)?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.cannot_be_a_base() {
            return Err(SessionsManagerClientError::InvalidBaseUrlScheme(endpoint));
        }
        endpoint
            .path_segments_mut()
            .map_err(|_| SessionsManagerClientError::InvalidBaseUrl)?
            .pop_if_empty()
            .extend(TOKEN_EXCHANGE_SEGMENTS);

        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(TOKEN_EXCHANGE_TIMEOUT)
                .build()?,
            endpoint,
            api_key: SecretString::from(api_key),
            cached: Mutex::new(None),
        })
    }

    async fn authorization(&self) -> Result<HeaderValue, SessionsManagerClientError> {
        let mut cached = self.cached.lock().await;

        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.refresh_at > SystemTime::now())
        {
            return Ok(token.header.clone());
        }

        let token = self.exchange().await?;
        let header = token.header.clone();
        *cached = Some(token);

        Ok(header)
    }

    async fn exchange(&self) -> Result<CachedToken, SessionsManagerClientError> {
        tracing::debug!(
            endpoint = %self.endpoint,
            "exchanging the MetalBear API key for a sessions-manager token"
        );

        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&TokenExchangeRequest {
                api_key: self.api_key.expose_secret(),
            })
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SessionsManagerClientError::ApiKeyRejected);
        }
        if !status.is_success() {
            return Err(SessionsManagerClientError::TokenExchangeStatus(status));
        }

        // Deliberately not `Response::json`: a malformed body is a broken server rather than a
        // transient fault, and decoding it through `reqwest` would surface as a retryable error.
        let body = response.bytes().await?;
        let TokenExchangeResponse { token } = serde_json::from_slice(&body).map_err(|_| {
            SessionsManagerClientError::TokenExchange(
                "response did not contain a `token`".to_owned(),
            )
        })?;

        let expires_at = token_expiry(&token)?;
        let mut header = HeaderValue::try_from(format!("Bearer {token}")).map_err(|_| {
            SessionsManagerClientError::TokenExchange(
                "token is not a valid header value".to_owned(),
            )
        })?;
        header.set_sensitive(true);

        tracing::debug!(
            ?expires_at,
            "obtained a sessions-manager token from the MetalBear cloud"
        );

        Ok(CachedToken {
            header,
            // Saturating, so a token that is already inside the margin is simply never reused.
            refresh_at: expires_at
                .checked_sub(TOKEN_REFRESH_MARGIN)
                .unwrap_or(SystemTime::UNIX_EPOCH),
        })
    }
}

/// Reads `exp` out of a JWT payload *without verifying the signature*.
///
/// The client holds no public key and could not verify one; authenticity comes from TLS to the
/// MetalBear cloud. The claim is read only to decide when to ask for a new token, so a payload
/// this client mis-trusts costs at most an ill-timed refresh — every party that acts on the
/// token verifies it properly.
fn token_expiry(token: &str) -> Result<SystemTime, SessionsManagerClientError> {
    let mut segments = token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(SessionsManagerClientError::TokenExchange(
            "token is not a JWT".to_owned(),
        ));
    };

    let payload = BASE64_URL_SAFE_NO_PAD.decode(payload).map_err(|_| {
        SessionsManagerClientError::TokenExchange(
            "token payload is not base64url-encoded".to_owned(),
        )
    })?;
    let claims: TokenClaims = serde_json::from_slice(&payload).map_err(|_| {
        SessionsManagerClientError::TokenExchange("token payload has no `exp` claim".to_owned())
    })?;

    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(claims.exp))
}

impl CredentialProvider for CloudTokenCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            headers.insert(reqwest::header::AUTHORIZATION, self.authorization().await?);
            Ok(headers)
        }
        .boxed()
    }

    /// Nothing: the data-plane upgrade authenticates with the single-use credential minted for
    /// its assignment, which occupies this same header name.
    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(HeaderMap::new())
    }
}

/// Whatever the environment configures, which may be both mechanisms at once: they answer to
/// different parties (a fronting proxy, and sessions-manager itself) and are enabled by
/// different variables, so neither implies nor excludes the other.
struct EnvCredentials {
    shared_secret: Option<SharedSecretCredentials>,
    cloud: Option<CloudTokenCredentials>,
}

impl CredentialProvider for EnvCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            if let Some(shared_secret) = &self.shared_secret {
                headers.extend(shared_secret.control_plane_headers().await?);
            }
            if let Some(cloud) = &self.cloud {
                headers.extend(cloud.control_plane_headers().await?);
            }
            Ok(headers)
        }
        .boxed()
    }

    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            if let Some(shared_secret) = &self.shared_secret {
                headers.extend(shared_secret.data_plane_headers().await?);
            }
            if let Some(cloud) = &self.cloud {
                headers.extend(cloud.data_plane_headers().await?);
            }
            Ok(headers)
        }
        .boxed()
    }
}

/// The credentials a client uses unless the caller supplies its own.
pub(crate) fn credentials_from_env()
-> Result<Arc<dyn CredentialProvider>, SessionsManagerClientError> {
    let shared_secret = SharedSecretCredentials::from_env()?;
    let cloud = CloudTokenCredentials::from_env()?;

    if let Some(shared_secret) = &shared_secret {
        tracing::debug!(
            header = %shared_secret.name,
            "authenticating sessions-manager connections with a shared secret"
        );
    }
    if let Some(cloud) = &cloud {
        tracing::debug!(
            endpoint = %cloud.endpoint,
            "authenticating sessions-manager control-plane requests with a MetalBear cloud token"
        );
    }

    match (shared_secret, cloud) {
        (None, None) => Ok(Arc::new(NoCredentials)),
        (shared_secret, cloud) => Ok(Arc::new(EnvCredentials {
            shared_secret,
            cloud,
        })),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        net::{Ipv4Addr, SocketAddr},
        sync::Mutex as BlockingMutex,
        time::UNIX_EPOCH,
    };

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use serde_json::{Value, json};

    use super::*;

    /// Exercises `new` rather than `from_env`, so the cases do not race each other over
    /// process environment.
    fn header_of(name: &str, token: &str) -> Result<HeaderMap, SessionsManagerClientError> {
        Ok(SharedSecretCredentials::new(name, token)?.header_map())
    }

    #[test]
    fn sends_the_token_under_the_configured_name() {
        let headers = header_of("x-custom-auth", "shhh").unwrap();
        assert_eq!(headers.get("x-custom-auth").unwrap(), "shhh");
    }

    #[test]
    fn header_name_is_case_insensitive() {
        let headers = header_of("X-Custom-Auth", "shhh").unwrap();
        assert_eq!(headers.get("x-custom-auth").unwrap(), "shhh");
    }

    #[test]
    fn token_is_marked_sensitive_so_it_stays_out_of_logs() {
        let headers = header_of("x-custom-auth", "shhh").unwrap();
        assert!(headers.get("x-custom-auth").unwrap().is_sensitive());
    }

    /// A reserved name is refused outright: the client overwrites each of these after
    /// applying credentials, so the secret would never reach the proxy.
    #[test]
    fn reserved_header_names_are_refused() {
        for name in RESERVED_HEADER_NAMES {
            // `SharedSecretCredentials` holds a secret and deliberately has no `Debug`, so
            // the result is matched rather than unwrapped.
            let Err(error) = SharedSecretCredentials::new(name, "shhh") else {
                panic!("{name} should be refused as an auth header name");
            };
            assert!(
                matches!(error, SessionsManagerClientError::InvalidConfig(_)),
                "{name} produced {error:?}"
            );
        }
    }

    #[test]
    fn reserved_check_ignores_case() {
        assert!(
            SharedSecretCredentials::new("Authorization", "shhh").is_err(),
            "reserved names should be refused whatever their case"
        );
    }

    #[test]
    fn malformed_names_and_tokens_are_refused() {
        assert!(
            SharedSecretCredentials::new("no spaces allowed", "shhh").is_err(),
            "an invalid header name should be refused"
        );
        assert!(
            SharedSecretCredentials::new("x-custom-auth", "new\nline").is_err(),
            "a token that cannot be a header value should be refused"
        );
    }

    const TEST_API_KEY: &str = "metalbear_key_abc123";

    /// A scripted stand-in for the MetalBear token endpoint.
    ///
    /// Every request pops the next queued response, so a test asserts how many exchanges
    /// happened simply by what is left in the queue, and an unscripted request panics rather
    /// than silently succeeding.
    struct TokenEndpointState {
        responses: BlockingMutex<VecDeque<(StatusCode, Value)>>,
        requests: BlockingMutex<Vec<Value>>,
    }

    struct TokenEndpoint {
        state: Arc<TokenEndpointState>,
        origin: String,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for TokenEndpoint {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl TokenEndpoint {
        async fn start(responses: impl IntoIterator<Item = (StatusCode, Value)>) -> Self {
            let state = Arc::new(TokenEndpointState {
                responses: BlockingMutex::new(responses.into_iter().collect()),
                requests: BlockingMutex::new(Vec::new()),
            });
            let app = Router::new()
                .route("/api/v2/token", post(Self::handle))
                .with_state(state.clone());
            let listener =
                tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                    .await
                    .unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });

            Self {
                state,
                origin,
                server,
            }
        }

        async fn handle(
            State(state): State<Arc<TokenEndpointState>>,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            state.requests.lock().unwrap().push(body);
            let (status, body) = state
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("token endpoint was called more times than the test scripted");
            (status, Json(body))
        }

        fn credentials(&self) -> CloudTokenCredentials {
            CloudTokenCredentials::new(&self.origin, TEST_API_KEY.to_owned()).unwrap()
        }

        fn exchanges(&self) -> usize {
            self.state.requests.lock().unwrap().len()
        }
    }

    /// A token whose payload carries the claims sessions-manager issues. Only `exp` is read by
    /// the client, and the signature is never checked, so it needs no key.
    fn token(expires_in: Duration) -> Value {
        let exp = (SystemTime::now() + expires_in)
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = BASE64_URL_SAFE_NO_PAD
            .encode(json!({ "sub": "org-1", "scope": "agent", "exp": exp }).to_string());

        json!({ "token": format!("eyJhbGciOiJFUzI1NiJ9.{payload}.c2lnbmF0dXJl") })
    }

    fn bearer(headers: &HeaderMap) -> &HeaderValue {
        headers
            .get(reqwest::header::AUTHORIZATION)
            .expect("cloud credentials should send an authorization header")
    }

    #[tokio::test]
    async fn exchanges_the_api_key_as_the_server_expects() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;

        let headers = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .unwrap();

        assert_eq!(
            endpoint.state.requests.lock().unwrap().as_slice(),
            [json!({ "apiKey": TEST_API_KEY })]
        );
        assert!(
            bearer(&headers).to_str().unwrap().starts_with("Bearer eyJ"),
            "the token should be sent as a bearer credential"
        );
    }

    #[tokio::test]
    async fn a_fresh_token_is_fetched_once_and_reused() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;
        let credentials = endpoint.credentials();

        let first = credentials.control_plane_headers().await.unwrap();
        let second = credentials.control_plane_headers().await.unwrap();

        assert_eq!(endpoint.exchanges(), 1, "the cached token should be reused");
        assert_eq!(bearer(&first), bearer(&second));
    }

    /// The first token expires inside [`TOKEN_REFRESH_MARGIN`], so it is replaced on the next
    /// use — and the replacement, which is not, is then reused like any other fresh token.
    #[tokio::test]
    async fn a_token_near_expiry_triggers_exactly_one_refresh() {
        let endpoint = TokenEndpoint::start([
            (StatusCode::OK, token(TOKEN_REFRESH_MARGIN / 2)),
            (StatusCode::OK, token(Duration::from_secs(600))),
        ])
        .await;
        let credentials = endpoint.credentials();

        let expiring = credentials.control_plane_headers().await.unwrap();
        let refreshed = credentials.control_plane_headers().await.unwrap();
        let reused = credentials.control_plane_headers().await.unwrap();

        assert_eq!(endpoint.exchanges(), 2);
        assert_ne!(bearer(&expiring), bearer(&refreshed));
        assert_eq!(bearer(&refreshed), bearer(&reused));
    }

    #[tokio::test]
    async fn a_rejected_api_key_is_reported_as_such_and_not_retried() {
        let endpoint = TokenEndpoint::start([(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "unknown api key" }),
        )])
        .await;

        let error = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .expect_err("a 401 should fail the exchange");

        assert!(
            matches!(error, SessionsManagerClientError::ApiKeyRejected),
            "expected a rejected-key error, got {error:?}"
        );
        assert!(
            !error.is_retryable(),
            "a permanently bad key must not be retried"
        );
    }

    /// Anything else the endpoint returns is a fault of the endpoint, not of the key, and keeps
    /// the retryability the status implies.
    #[tokio::test]
    async fn a_failing_token_endpoint_stays_retryable() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::BAD_GATEWAY, json!({ "error": "upstream" }))]).await;

        let error = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .expect_err("a 502 should fail the exchange");

        assert!(
            matches!(
                error,
                SessionsManagerClientError::TokenExchangeStatus(StatusCode::BAD_GATEWAY)
            ),
            "expected a token exchange status error, got {error:?}"
        );
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn a_response_that_is_not_a_jwt_is_rejected() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, json!({ "token": "not-a-jwt" }))]).await;

        let error = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .expect_err("a malformed token should fail the exchange");

        assert!(
            matches!(error, SessionsManagerClientError::TokenExchange(_)),
            "expected a token exchange error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn the_bearer_token_is_marked_sensitive_so_it_stays_out_of_logs() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;

        let headers = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .unwrap();

        assert!(bearer(&headers).is_sensitive());
    }

    /// The per-assignment credential owns `authorization` on the data plane, so the cloud token
    /// must not be offered there at all.
    #[tokio::test]
    async fn the_cloud_token_never_reaches_the_data_plane() {
        let endpoint = TokenEndpoint::start([]).await;

        let headers = endpoint.credentials().data_plane_headers().await.unwrap();

        assert!(headers.is_empty());
        assert_eq!(
            endpoint.exchanges(),
            0,
            "the data plane should not even trigger an exchange"
        );
    }

    /// Both mechanisms configured at once: the proxy secret goes to both planes, the cloud token
    /// only to the control plane.
    #[tokio::test]
    async fn both_mechanisms_can_be_configured_at_once() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;
        let credentials = EnvCredentials {
            shared_secret: Some(
                SharedSecretCredentials::new(DEFAULT_AUTH_HEADER_NAME, "shhh").unwrap(),
            ),
            cloud: Some(endpoint.credentials()),
        };

        let control_plane = credentials.control_plane_headers().await.unwrap();
        let data_plane = credentials.data_plane_headers().await.unwrap();

        assert_eq!(control_plane.get(DEFAULT_AUTH_HEADER_NAME).unwrap(), "shhh");
        assert!(control_plane.contains_key(reqwest::header::AUTHORIZATION));
        assert_eq!(data_plane.get(DEFAULT_AUTH_HEADER_NAME).unwrap(), "shhh");
        assert!(!data_plane.contains_key(reqwest::header::AUTHORIZATION));
    }

    #[test]
    fn a_key_without_the_metalbear_prefix_is_refused() {
        let Err(error) =
            CloudTokenCredentials::new("https://app.metalbear.com", "hunter2".to_owned())
        else {
            panic!("a key without the MetalBear prefix should be refused");
        };
        assert!(
            matches!(error, SessionsManagerClientError::InvalidConfig(_)),
            "expected a config error, got {error:?}"
        );
    }

    #[test]
    fn the_exchange_endpoint_is_appended_to_the_configured_origin() {
        let endpoint = |origin| {
            CloudTokenCredentials::new(origin, TEST_API_KEY.to_owned())
                .unwrap()
                .endpoint
                .to_string()
        };

        assert_eq!(
            endpoint("https://app.staging.metalbear.com"),
            "https://app.staging.metalbear.com/api/v2/token"
        );
        assert_eq!(
            endpoint("https://app.staging.metalbear.com/"),
            "https://app.staging.metalbear.com/api/v2/token"
        );
    }

    /// Without an API key the provider is not built at all, and a client with neither mechanism
    /// configured sends no credential headers.
    #[tokio::test]
    async fn is_a_no_op_when_the_api_key_is_unset() {
        for name in [
            SESSIONS_MANAGER_API_KEY_ENV,
            SESSIONS_MANAGER_AUTH_TOKEN_ENV,
        ] {
            assert!(
                std::env::var(name).is_err(),
                "{name} must not be set while these tests run"
            );
        }

        assert!(CloudTokenCredentials::from_env().unwrap().is_none());
        assert!(
            credentials_from_env()
                .unwrap()
                .control_plane_headers()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
