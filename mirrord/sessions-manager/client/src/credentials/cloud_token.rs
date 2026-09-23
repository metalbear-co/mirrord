//! A short-lived MetalBear cloud token, exchanged for a long-lived API key.

use std::{env::VarError, time::Duration};

use futures::future::{BoxFuture, FutureExt};
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use super::{CredentialProvider, ready_headers};
use crate::error::SessionsManagerClientError;

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

/// Authenticates the client to sessions-manager itself, by trading a long-lived MetalBear API
/// key for a short-lived token and sending it as `Authorization: Bearer <token>`.
///
/// The exchange endpoint is unauthenticated apart from the key in the request body, so the key
/// must never leave this process by any other route — neither it nor the tokens it yields are
/// ever logged.
///
/// The token is opaque to the client: it is neither verified nor parsed here, since
/// sessions-manager is the one that checks it. With nothing to learn its lifetime from, a fresh
/// token is exchanged for every request that needs one. That is cheap because those requests are
/// rare — sessions-manager connections are long-lived, and the token is only needed to open one.
///
/// Control plane only: the data-plane upgrade already sends the single-use per-assignment
/// credential under `authorization`, and must not have it replaced.
pub struct CloudTokenCredentials {
    client: reqwest::Client,
    pub(super) endpoint: Url,
    api_key: SecretString,
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
        })
    }

    /// Exchanges the API key for a token, returned as a complete `Bearer <token>` header value
    /// marked sensitive.
    async fn exchange(&self) -> Result<HeaderValue, SessionsManagerClientError> {
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
        let TokenExchangeResponse { token } = serde_json::from_slice(&body)
            .map_err(|_| SessionsManagerClientError::TokenExchangeMissingToken)?;

        let mut header = HeaderValue::try_from(format!("Bearer {token}"))
            .map_err(|_| SessionsManagerClientError::TokenNotHeaderValue)?;
        header.set_sensitive(true);

        tracing::debug!("obtained a sessions-manager token from the MetalBear cloud");

        Ok(header)
    }
}

/// Whether the API key may be sent to `url`. It is long-lived and travels in the request body,
/// so anything but TLS exposes it to the network; plain HTTP is tolerated only on loopback,
/// where there is no network to expose it to (tests, or a locally-run app-server).
///
/// `allow_any_http` lifts the loopback restriction for debug builds, so a developer can point a
/// locally-built agent at an app-server reachable only over plain HTTP. Release builds, the only
/// ones that ship, never do.
fn is_secure_cloud_url(url: &Url, allow_any_http: bool) -> bool {
    if url.cannot_be_a_base() {
        return false;
    }

    match url.scheme() {
        "https" => true,
        "http" if allow_any_http => true,
        "http" => match url.host() {
            Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

impl CredentialProvider for CloudTokenCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            headers.insert(reqwest::header::AUTHORIZATION, self.exchange().await?);
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

        pub(in crate::credentials) fn credentials(&self) -> CloudTokenCredentials {
            CloudTokenCredentials::new(&self.origin, TEST_API_KEY.to_owned()).unwrap()
        }

        fn exchanges(&self) -> usize {
            self.state.requests.lock().unwrap().len()
        }
    }

    /// A token endpoint response carrying `token`, which the client passes on without reading.
    pub(in crate::credentials) fn token(token: &str) -> Value {
        json!({ "token": token })
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

    /// Whatever the token is, including something that is not a JWT, is sent as-is: judging it
    /// is up to sessions-manager.
    #[tokio::test]
    async fn every_request_exchanges_a_fresh_opaque_token() {
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
    fn plain_http_is_accepted_only_on_loopback_unless_allowed() {
        let secure = |origin, allow_any_http| {
            is_secure_cloud_url(&Url::parse(origin).unwrap(), allow_any_http)
        };

        for allow_any_http in [false, true] {
            assert!(secure("https://app.metalbear.com", allow_any_http));
            assert!(secure("http://localhost:8080", allow_any_http));
            assert!(secure("http://127.0.0.1:8080", allow_any_http));
            assert!(secure("http://[::1]:8080", allow_any_http));
            assert!(!secure("ftp://app.metalbear.com", allow_any_http));
            assert!(!secure("mailto:dev@metalbear.com", allow_any_http));
        }

        for origin in ["http://app.metalbear.com", "http://10.0.0.1"] {
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
