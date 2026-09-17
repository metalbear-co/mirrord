//! A short-lived MetalBear cloud token, exchanged for a long-lived API key.

use std::{
    env::VarError,
    time::{Duration, SystemTime},
};

use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use futures::future::{BoxFuture, FutureExt};
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use super::{CredentialProvider, ready_headers};
use crate::error::SessionsManagerClientError;

/// Long-lived MetalBear API key, exchanged for the short-lived token sessions-manager verifies.
/// Setting it is what turns [`CloudTokenCredentials`] on.
pub const SESSIONS_MANAGER_API_KEY_ENV: &str = "MIRRORD_SESSIONS_MANAGER_API_KEY";

/// Origin of the MetalBear cloud that mints tokens, for pointing a client at somewhere other
/// than [`METALBEAR_CLOUD_URL_DEFAULT`] (staging, or a mock in tests).
pub const METALBEAR_CLOUD_URL_ENV: &str = "MIRRORD_METALBEAR_CLOUD_URL";

const METALBEAR_CLOUD_URL_DEFAULT: &str = "https://app.metalbear.com";

/// Development-only mirrord session key. When set, the token exchange is tagged with
/// `baggage: mirrord-session=<value>` so it routes to a locally-run app-server under mirrord
/// instead of the deployed one — the operator's `OPERATOR_CLOUD_BAGGAGE_SESSION` for the same
/// purpose. Unset in production.
pub const METALBEAR_CLOUD_BAGGAGE_SESSION_ENV: &str = "MIRRORD_METALBEAR_CLOUD_BAGGAGE_SESSION";

/// Name of the W3C baggage header, which mirrord's traffic-stealing filter matches on.
const BAGGAGE_HEADER: &str = "baggage";

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

/// Scope of the API key agents and intproxies are meant to hold. Enforcing it is the job of the
/// server that verifies the token; the client only warns on a mismatch, so that a key issued for
/// the wrong purpose is diagnosable from the agent's own logs instead of only from a rejection.
const EXPECTED_TOKEN_SCOPE: &str = "serverless_agent";

/// The claims this client reads. The rest (`sub`, `aud`, ...) are for the server that verifies
/// the token.
#[derive(Deserialize)]
struct TokenClaims {
    exp: u64,
    /// Tokens minted before scopes existed omit it.
    scope: Option<String>,
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
    pub(super) endpoint: Url,
    api_key: SecretString,
    cached: Mutex<Option<CachedToken>>,
    /// Development-only; see [`METALBEAR_CLOUD_BAGGAGE_SESSION_ENV`]. Held as a finished header
    /// value so a session key that cannot be one is refused at construction rather than failing
    /// every exchange.
    baggage: Option<HeaderValue>,
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
        let baggage_session = std::env::var(METALBEAR_CLOUD_BAGGAGE_SESSION_ENV)
            .ok()
            .filter(|session| !session.trim().is_empty());

        Self::new(&cloud_url, api_key)?
            .with_baggage_session(baggage_session.as_deref())
            .map(Some)
    }

    pub fn new(cloud_url: &str, api_key: String) -> Result<Self, SessionsManagerClientError> {
        if !api_key.starts_with(API_KEY_PREFIX) {
            return Err(SessionsManagerClientError::InvalidApiKey);
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
            baggage: None,
        })
    }

    /// Development-only: routes the token exchange to a locally-run app-server under mirrord by
    /// tagging it with `baggage: mirrord-session=<session>`. A no-op when `session` is [`None`].
    ///
    /// Only the exchange is tagged. Requests to sessions-manager itself are addressed by
    /// `MIRRORD_SESSIONS_MANAGER_URL`, which already points wherever a developer wants them.
    pub fn with_baggage_session(
        mut self,
        session: Option<&str>,
    ) -> Result<Self, SessionsManagerClientError> {
        self.baggage = session
            .map(|session| {
                HeaderValue::from_str(&format!("mirrord-session={session}"))
                    .map_err(|_| SessionsManagerClientError::InvalidCloudBaggageSession)
            })
            .transpose()?;

        Ok(self)
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

        let mut request = self
            .client
            .post(self.endpoint.clone())
            .json(&TokenExchangeRequest {
                api_key: self.api_key.expose_secret(),
            });
        if let Some(baggage) = &self.baggage {
            request = request.header(BAGGAGE_HEADER, baggage);
        }

        let response = request.send().await?;

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

        let claims = token_claims(&token)?;
        if claims.scope.as_deref() != Some(EXPECTED_TOKEN_SCOPE) {
            tracing::warn!(
                scope = ?claims.scope,
                expected = EXPECTED_TOKEN_SCOPE,
                "the MetalBear API key is not a serverless agent key, sessions-manager is \
                 expected to reject the token it was exchanged for; check \
                 {SESSIONS_MANAGER_API_KEY_ENV}"
            );
        }
        let expires_at = SystemTime::UNIX_EPOCH + Duration::from_secs(claims.exp);

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

/// Reads [`TokenClaims`] out of a JWT payload *without verifying the signature*.
///
/// The client holds no public key and could not verify one; authenticity comes from TLS to the
/// MetalBear cloud. The claims are read only to decide when to ask for a new token and whether to
/// warn about the key's scope, so a payload this client mis-trusts costs at most an ill-timed
/// refresh or a spurious warning — every party that acts on the token verifies it properly.
fn token_claims(token: &str) -> Result<TokenClaims, SessionsManagerClientError> {
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
    serde_json::from_slice(&payload).map_err(|_| {
        SessionsManagerClientError::TokenExchange("token payload has no `exp` claim".to_owned())
    })
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

/// Also used by the parent module's tests, which combine these credentials with others.
#[cfg(test)]
pub(super) mod tests {
    use std::{
        collections::VecDeque,
        net::{Ipv4Addr, SocketAddr},
        sync::{Arc, Mutex as BlockingMutex},
        time::UNIX_EPOCH,
    };

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use serde_json::{Value, json};

    use super::*;

    const TEST_API_KEY: &str = "metalbear_key_abc123";

    /// A scripted stand-in for the MetalBear token endpoint.
    ///
    /// Every request pops the next queued response, so a test asserts how many exchanges
    /// happened simply by what is left in the queue, and an unscripted request panics rather
    /// than silently succeeding.
    struct TokenEndpointState {
        responses: BlockingMutex<VecDeque<(StatusCode, Value)>>,
        requests: BlockingMutex<Vec<Value>>,
        baggage: BlockingMutex<Vec<Option<String>>>,
    }

    pub(in crate::credentials) struct TokenEndpoint {
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
        pub(in crate::credentials) async fn start(
            responses: impl IntoIterator<Item = (StatusCode, Value)>,
        ) -> Self {
            let state = Arc::new(TokenEndpointState {
                responses: BlockingMutex::new(responses.into_iter().collect()),
                requests: BlockingMutex::new(Vec::new()),
                baggage: BlockingMutex::new(Vec::new()),
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
            headers: reqwest::header::HeaderMap,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            state.baggage.lock().unwrap().push(
                headers
                    .get("baggage")
                    .map(|value| value.to_str().unwrap().to_owned()),
            );
            state.requests.lock().unwrap().push(body);
            let (status, body) = state
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("token endpoint was called more times than the test scripted");
            (status, Json(body))
        }

        pub(in crate::credentials) fn credentials(&self) -> CloudTokenCredentials {
            CloudTokenCredentials::new(&self.origin, TEST_API_KEY.to_owned()).unwrap()
        }

        fn exchanges(&self) -> usize {
            self.state.requests.lock().unwrap().len()
        }

        /// The `baggage` header seen on each exchange, in order.
        fn baggage(&self) -> Vec<Option<String>> {
            self.state.baggage.lock().unwrap().clone()
        }
    }

    /// A token whose payload carries the claims sessions-manager issues. Only `exp` is read by
    /// the client, and the signature is never checked, so it needs no key.
    pub(in crate::credentials) fn token(expires_in: Duration) -> Value {
        token_with_scope(expires_in, Some(EXPECTED_TOKEN_SCOPE))
    }

    fn token_with_scope(expires_in: Duration, scope: Option<&str>) -> Value {
        let exp = (SystemTime::now() + expires_in)
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = BASE64_URL_SAFE_NO_PAD
            .encode(json!({ "sub": "org-1", "scope": scope, "exp": exp }).to_string());

        json!({ "token": format!("eyJhbGciOiJFUzI1NiJ9.{payload}.c2lnbmF0dXJl") })
    }

    /// The scope check only warns: a token of another scope, or of none, is still handed to
    /// sessions-manager, which is the one that decides whether to accept it.
    #[tokio::test]
    async fn a_token_of_another_scope_is_still_used() {
        let endpoint = TokenEndpoint::start([
            (
                StatusCode::OK,
                token_with_scope(Duration::from_secs(600), Some("operator")),
            ),
            (
                StatusCode::OK,
                token_with_scope(Duration::from_secs(600), None),
            ),
        ])
        .await;

        for _ in 0..2 {
            let headers = endpoint
                .credentials()
                .control_plane_headers()
                .await
                .unwrap();
            assert!(headers.contains_key(reqwest::header::AUTHORIZATION));
        }
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

    /// The whole point of the session key: a developer running app-server locally under mirrord
    /// needs the exchange to carry the baggage their steal filter matches on.
    #[tokio::test]
    async fn a_configured_session_routes_the_exchange_to_a_local_app_server() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;

        endpoint
            .credentials()
            .with_baggage_session(Some("mbe2092"))
            .unwrap()
            .control_plane_headers()
            .await
            .unwrap();

        assert_eq!(
            endpoint.baggage(),
            [Some("mirrord-session=mbe2092".to_owned())]
        );
    }

    /// Production sends no baggage at all, so nothing about a deployed agent's traffic invites
    /// interception.
    #[tokio::test]
    async fn no_session_means_no_baggage_header() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;

        endpoint
            .credentials()
            .with_baggage_session(None)
            .unwrap()
            .control_plane_headers()
            .await
            .unwrap();

        assert_eq!(endpoint.baggage(), [None]);
    }

    #[test]
    fn a_session_that_cannot_be_a_header_value_is_refused() {
        let credentials = CloudTokenCredentials::new(
            "https://app.staging.metalbear.com",
            TEST_API_KEY.to_owned(),
        )
        .unwrap();

        let Err(error) = credentials.with_baggage_session(Some("new\nline")) else {
            panic!("a session key that cannot be a header value should be refused");
        };
        assert!(
            matches!(
                error,
                SessionsManagerClientError::InvalidCloudBaggageSession
            ),
            "expected a config error, got {error:?}"
        );
    }

    #[test]
    fn a_key_without_the_metalbear_prefix_is_refused() {
        let Err(error) =
            CloudTokenCredentials::new("https://app.metalbear.com", "hunter2".to_owned())
        else {
            panic!("a key without the MetalBear prefix should be refused");
        };
        assert!(
            matches!(error, SessionsManagerClientError::InvalidApiKey),
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
}
