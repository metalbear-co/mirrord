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
use tokio::{sync::Mutex, time::Instant};
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

const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long before a token's `exp` it stops being reused. Covers the request the token is about
/// to be used for, plus clock skew between this process and sessions-manager.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Upper bound on reusing one token, whatever its `exp` claims. Tokens are minted for minutes;
/// an `exp` far beyond that is not something to keep presenting on the strength of an unverified
/// claim.
const MAX_TOKEN_REUSE: Duration = Duration::from_secs(60 * 60);

#[derive(Serialize)]
struct TokenExchangeRequest<'a> {
    #[serde(rename = "apiKey")]
    api_key: &'a str,
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    token: String,
}

/// The one claim this client reads, to schedule the next exchange. The token's expiry is only
/// carried here: the exchange response has no separate field for it.
#[derive(Deserialize)]
struct TokenClaims {
    exp: u64,
}

struct CachedToken {
    /// Complete `Bearer <token>` value, marked sensitive.
    header: HeaderValue,
    /// Monotonic, so a wall-clock jump after the exchange can't extend the token's reuse.
    refresh_at: Instant,
}

/// Authenticates the client to sessions-manager itself, by trading a long-lived MetalBear API
/// key for a short-lived token and sending it as `Authorization: Bearer <token>`.
///
/// The exchange endpoint is unauthenticated apart from the key in the request body, so the key
/// must never leave this process by any other route — neither it nor the tokens it yields are
/// ever logged.
///
/// The token is not verified here, since sessions-manager is the one that checks it. Its `exp`
/// is read only to decide how long to reuse it, which lets a reconnect ride out a cloud outage
/// on a token that is still valid. A token whose `exp` can't be read is still sent, just never
/// reused. The mutex is held across the exchange so that concurrent callers waiting on a cold or
/// stale cache collapse into a single request rather than each starting one of their own.
///
/// Control plane only. The data plane is authenticated through the control plane rather than
/// alongside it: the single-use per-assignment credential the upgrade presents is minted by a
/// control-plane request that this token already authenticated, so it vouches for the same
/// caller while also binding the connection to one assignment and one consumer. That credential
/// owns `authorization` on the upgrade, and must not have it replaced.
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

        let cloud_url = cloud_url_from_env()?;
        // An unreadable session must fail loudly: ignoring it would route the exchange to the
        // deployed app-server while the developer waits on their local one.
        let baggage_session = match std::env::var(METALBEAR_CLOUD_BAGGAGE_SESSION_ENV) {
            Ok(session) => Some(session).filter(|session| !session.trim().is_empty()),
            Err(VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        };

        Self::new(&cloud_url, api_key)?
            .with_baggage_session(baggage_session.as_deref())
            .map(Some)
    }

    pub fn new(cloud_url: &str, api_key: String) -> Result<Self, SessionsManagerClientError> {
        if !api_key.starts_with(API_KEY_PREFIX) {
            return Err(SessionsManagerClientError::InvalidApiKey);
        }

        let mut endpoint = Url::parse(cloud_url)?;
        if !is_secure_cloud_url(&endpoint, cfg!(debug_assertions)) {
            return Err(SessionsManagerClientError::InsecureCloudUrl(endpoint));
        }
        endpoint
            .path_segments_mut()
            .map_err(|_| SessionsManagerClientError::InvalidBaseUrl)?
            .pop_if_empty()
            .extend(TOKEN_EXCHANGE_SEGMENTS);

        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(TOKEN_EXCHANGE_TIMEOUT)
                // A 307/308 replays the POST body, API key included, to wherever it points, and
                // reqwest only strips sensitive *headers* across origins. The token endpoint is a
                // fixed path that has no reason to redirect.
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            api_key: SecretString::from(api_key),
            cached: Mutex::new(None),
            baggage: None,
        })
    }

    async fn authorization(&self) -> Result<HeaderValue, SessionsManagerClientError> {
        let mut cached = self.cached.lock().await;

        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.refresh_at > Instant::now())
        {
            return Ok(token.header.clone());
        }

        let header = self.exchange().await?;
        *cached = token_reuse(&header).map(|reuse| CachedToken {
            header: header.clone(),
            refresh_at: Instant::now() + reuse,
        });

        Ok(header)
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

    /// Exchanges the API key for a token, returned as a complete `Bearer <token>` header value
    /// marked sensitive.
    async fn exchange(&self) -> Result<HeaderValue, SessionsManagerClientError> {
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
        let TokenExchangeResponse { token } = serde_json::from_slice(&body)
            .map_err(|_| SessionsManagerClientError::TokenExchangeMissingToken)?;

        let mut header = HeaderValue::try_from(format!("Bearer {token}"))
            .map_err(|_| SessionsManagerClientError::TokenNotHeaderValue)?;
        header.set_sensitive(true);

        tracing::debug!("obtained a sessions-manager token from the MetalBear cloud");

        Ok(header)
    }
}

/// How long the token in `header` may be reused, or [`None`] when it shouldn't be.
///
/// Reads `exp` from the JWT payload *without verifying the signature*: the client holds no key to
/// verify it with, and sessions-manager verifies the token anyway. A claim this client mis-trusts
/// therefore costs at most an ill-timed exchange, and [`MAX_TOKEN_REUSE`] bounds how long a bogus
/// one can keep a token in use.
fn token_reuse(header: &HeaderValue) -> Option<Duration> {
    let token = header.to_str().ok()?.strip_prefix("Bearer ")?;
    let [_header, payload, _signature] = token.split('.').collect::<Vec<_>>()[..] else {
        return None;
    };
    let TokenClaims { exp } =
        serde_json::from_slice(&BASE64_URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;

    let refresh_at = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(exp))?
        .checked_sub(TOKEN_REFRESH_MARGIN)?;
    let reuse = refresh_at.duration_since(SystemTime::now()).ok()?;

    Some(reuse.min(MAX_TOKEN_REUSE))
}

/// The MetalBear cloud this client talks to: [`METALBEAR_CLOUD_URL_ENV`], or production when it
/// is unset. Agents exchange their API key there, and it issued the `mirrord login` token an
/// intproxy presents.
///
/// An unreadable override is an error rather than a fallback to production, which would send the
/// API key, or look up the login token, for the wrong cloud.
pub fn cloud_url_from_env() -> Result<String, SessionsManagerClientError> {
    match std::env::var(METALBEAR_CLOUD_URL_ENV) {
        Ok(cloud_url) => Ok(cloud_url),
        Err(VarError::NotPresent) => Ok(METALBEAR_CLOUD_URL_DEFAULT.to_owned()),
        Err(error) => Err(error.into()),
    }
}

/// Whether the API key may be sent to `url`. It is long-lived and travels in the request body,
/// so anything but TLS exposes it to the network.
///
/// `allow_http` is set only in debug builds, so a developer can point a locally-built agent at a
/// test endpoint or an app-server reachable only over plain HTTP. Release builds, the only ones
/// that ship, require HTTPS without exception, loopback included.
fn is_secure_cloud_url(url: &Url, allow_http: bool) -> bool {
    !url.cannot_be_a_base()
        && match url.scheme() {
            "https" => true,
            "http" => allow_http,
            _ => false,
        }
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
    /// its assignment by a cloud-token-authenticated control-plane request, and that credential
    /// occupies this same header name.
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
    };

    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::Redirect,
        routing::{any, post},
    };
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

    /// A token endpoint response carrying `token`.
    pub(in crate::credentials) fn token(token: &str) -> Value {
        json!({ "token": token })
    }

    /// A JWT expiring `exp` seconds after the epoch. Only the payload is ever read, and the
    /// signature never checked, so it needs no key.
    fn jwt_expiring_at(exp: u64) -> String {
        let payload =
            BASE64_URL_SAFE_NO_PAD.encode(json!({ "sub": "org-1", "exp": exp }).to_string());
        format!("eyJhbGciOiJFUzI1NiJ9.{payload}.c2lnbmF0dXJl")
    }

    fn jwt_expiring_in(expires_in: Duration) -> String {
        let exp = (SystemTime::now() + expires_in)
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        jwt_expiring_at(exp)
    }

    fn bearer_value(token: &str) -> HeaderValue {
        HeaderValue::try_from(format!("Bearer {token}")).unwrap()
    }

    fn bearer(headers: &HeaderMap) -> &HeaderValue {
        headers
            .get(reqwest::header::AUTHORIZATION)
            .expect("cloud credentials should send an authorization header")
    }

    #[tokio::test]
    async fn exchanges_the_api_key_as_the_server_expects() {
        let endpoint = TokenEndpoint::start([(StatusCode::OK, token("abc"))]).await;

        let headers = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .unwrap();

        assert_eq!(
            endpoint.state.requests.lock().unwrap().as_slice(),
            [json!({ "apiKey": TEST_API_KEY })]
        );
        assert_eq!(bearer(&headers), "Bearer abc");
    }

    /// A token whose expiry is readable is reused, so a reconnect while the cloud is unreachable
    /// still has a credential to present.
    #[tokio::test]
    async fn a_token_is_reused_until_shortly_before_it_expires() {
        let token = jwt_expiring_in(Duration::from_secs(600));
        let endpoint = TokenEndpoint::start([(StatusCode::OK, self::token(&token))]).await;
        let credentials = endpoint.credentials();

        let first = credentials.control_plane_headers().await.unwrap();
        let second = credentials.control_plane_headers().await.unwrap();

        assert_eq!(endpoint.exchanges(), 1, "the cached token should be reused");
        assert_eq!(bearer(&first), bearer(&second));
    }

    /// The first token is already inside [`TOKEN_REFRESH_MARGIN`], so it is used once and then
    /// replaced, and the replacement is reused like any fresh token.
    #[tokio::test]
    async fn a_token_near_expiry_is_replaced_once() {
        let endpoint = TokenEndpoint::start([
            (
                StatusCode::OK,
                token(&jwt_expiring_in(TOKEN_REFRESH_MARGIN / 2)),
            ),
            (
                StatusCode::OK,
                token(&jwt_expiring_in(Duration::from_secs(600))),
            ),
        ])
        .await;
        let credentials = endpoint.credentials();

        let expiring = credentials.control_plane_headers().await.unwrap();
        let replaced = credentials.control_plane_headers().await.unwrap();
        let reused = credentials.control_plane_headers().await.unwrap();

        assert_eq!(endpoint.exchanges(), 2);
        assert_ne!(bearer(&expiring), bearer(&replaced));
        assert_eq!(bearer(&replaced), bearer(&reused));
    }

    #[test]
    fn reuse_is_capped_whatever_the_token_claims() {
        let far_future = bearer_value(&jwt_expiring_in(Duration::from_secs(30 * 24 * 60 * 60)));

        assert_eq!(token_reuse(&far_future), Some(MAX_TOKEN_REUSE));
    }

    /// An `exp` past what [`SystemTime`] can represent must not panic the agent or intproxy.
    #[test]
    fn an_unrepresentable_expiry_is_never_reused() {
        assert_eq!(token_reuse(&bearer_value(&jwt_expiring_at(u64::MAX))), None);
    }

    /// Whatever the token is, including something that is not a JWT, is sent as-is: judging it
    /// is up to sessions-manager. Without a readable expiry it is just never reused.
    #[tokio::test]
    async fn a_token_without_a_readable_expiry_is_sent_but_not_reused() {
        let endpoint = TokenEndpoint::start([
            (StatusCode::OK, token("first")),
            (StatusCode::OK, token("not.a.jwt")),
        ])
        .await;
        let credentials = endpoint.credentials();

        let first = credentials.control_plane_headers().await.unwrap();
        let second = credentials.control_plane_headers().await.unwrap();

        assert_eq!(endpoint.exchanges(), 2);
        assert_eq!(bearer(&first), "Bearer first");
        assert_eq!(bearer(&second), "Bearer not.a.jwt");
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
    async fn a_response_without_a_token_is_rejected() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, json!({ "error": "no token" }))]).await;

        let error = endpoint
            .credentials()
            .control_plane_headers()
            .await
            .expect_err("a response without a token should fail the exchange");

        assert!(
            matches!(error, SessionsManagerClientError::TokenExchangeMissingToken),
            "expected a missing-token error, got {error:?}"
        );
        assert!(
            !error.is_retryable(),
            "a broken token endpoint should not be retried"
        );
    }

    #[tokio::test]
    async fn the_bearer_token_is_marked_sensitive_so_it_stays_out_of_logs() {
        let endpoint = TokenEndpoint::start([(StatusCode::OK, token("abc"))]).await;

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
        let endpoint = TokenEndpoint::start([(StatusCode::OK, token("abc"))]).await;

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
        let endpoint = TokenEndpoint::start([(StatusCode::OK, token("abc"))]).await;

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

    /// A redirect would replay the body, API key included, to wherever the endpoint points it.
    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let followed = Arc::new(BlockingMutex::new(0));
        let app = Router::new()
            .route(
                "/api/v2/token",
                post(|| async { Redirect::temporary("/elsewhere") }),
            )
            .route(
                "/elsewhere",
                any({
                    let followed = followed.clone();
                    move || async move {
                        *followed.lock().unwrap() += 1;
                        Json(token("leaked"))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let error = CloudTokenCredentials::new(&origin, TEST_API_KEY.to_owned())
            .unwrap()
            .control_plane_headers()
            .await
            .expect_err("a redirect should fail the exchange");
        server.abort();

        assert_eq!(*followed.lock().unwrap(), 0, "the redirect was followed");
        assert!(
            matches!(
                error,
                SessionsManagerClientError::TokenExchangeStatus(StatusCode::TEMPORARY_REDIRECT)
            ),
            "expected the redirect status as an error, got {error:?}"
        );
    }

    #[test]
    fn plain_http_is_accepted_only_when_allowed() {
        let secure =
            |origin, allow_http| is_secure_cloud_url(&Url::parse(origin).unwrap(), allow_http);

        for allow_http in [false, true] {
            assert!(secure("https://app.metalbear.com", allow_http));
            assert!(!secure("ftp://app.metalbear.com", allow_http));
            assert!(!secure("mailto:dev@metalbear.com", allow_http));
        }

        for origin in [
            "http://app.metalbear.com",
            "http://10.0.0.1",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(!secure(origin, false), "{origin} should be refused");
            assert!(secure(origin, true), "{origin} should be allowed");
        }
    }

    #[test]
    fn an_insecure_cloud_url_is_refused_as_such() {
        assert!(matches!(
            CloudTokenCredentials::new("ftp://app.metalbear.com", TEST_API_KEY.to_owned()),
            Err(SessionsManagerClientError::InsecureCloudUrl(_))
        ));
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
