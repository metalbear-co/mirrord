//! A standalone sessions-manager reached at its own base URL.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{
    Request,
    header::{ACCEPT, AUTHORIZATION, HeaderMap},
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
use mirrord_sessions_manager_protocol::{
    AssignmentSubscription, ConnectionAssignment, ServiceScope,
};
use reqwest::StatusCode;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use url::Url;

use super::{SessionsManagerTransport, assignment_authorization};
use crate::{
    control_plane::{
        ControlPlaneEventStream,
        api::{ControlPlaneApi, ControlPlaneEndpoint},
        verify_event_stream,
    },
    credentials::{
        CredentialProvider, LoginTokenSource, NoCredentials, SESSIONS_MANAGER_URL_DEFAULT,
        credentials_from_env, intproxy_credentials_from_env,
    },
    error::SessionsManagerClientError,
    retry::with_deadline,
};

/// Complete sessions-manager API base URL, including any deployment-specific path prefix.
///
/// For example, both `https://example.com/sm` and `https://sm.example.com` are valid; API version
/// and resource segments are appended to the configured path.
pub const SESSIONS_MANAGER_URL_ENV: &str = "MIRRORD_SESSIONS_MANAGER_URL";

/// Why a standalone sessions-manager was chosen, so agents and the CLI can tell the user where a
/// session connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSource {
    /// Set by the user, through [`SESSIONS_MANAGER_URL_ENV`] or the CLI's
    /// `sessions_manager.url`.
    Explicit,
    /// Nothing selected a sessions-manager, so it is the MetalBear Cloud one at
    /// [`SESSIONS_MANAGER_URL_DEFAULT`].
    Default,
}

/// Base URL of the standalone sessions-manager to reach, and why it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsManagerEndpoint {
    pub url: Url,
    pub source: EndpointSource,
}

impl SessionsManagerEndpoint {
    /// The sessions-manager at `explicit` when it is set, and the MetalBear Cloud one otherwise.
    ///
    /// Choosing the operator-hosted sessions-manager instead comes first and is up to the caller,
    /// since agents and the CLI each select it in their own way.
    pub fn resolve(explicit: Option<&str>) -> Result<Self, SessionsManagerClientError> {
        let (url, source) = match explicit {
            Some(url) => (url, EndpointSource::Explicit),
            None => (SESSIONS_MANAGER_URL_DEFAULT, EndpointSource::Default),
        };

        Ok(Self {
            url: Url::parse(url)?,
            source,
        })
    }
}

/// Bound on establishing the TCP (and TLS) connection for control-plane HTTP requests.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on receiving response headers for the assignments subscription.
///
/// Covers only the initial response; the event stream body stays open without a deadline.
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the whole data-plane dial, from connecting through completing the WebSocket upgrade.
const WEBSOCKET_UPGRADE_TIMEOUT: Duration = Duration::from_secs(30);

/// Reaches a standalone sessions-manager over plain HTTP(S).
///
/// Every request carries the [`CredentialProvider`]'s headers, for deployments that front
/// sessions-manager with an authenticating proxy. The data-plane upgrade additionally carries the
/// assignment's own authorization in `Authorization`.
#[derive(Clone)]
pub struct DirectTransport {
    http: reqwest::Client,
    api: ControlPlaneApi,
    credentials: Arc<dyn CredentialProvider>,
}

impl DirectTransport {
    /// Reaches the sessions-manager at `base_url` without credentials.
    pub fn new(base_url: Url) -> Result<Self, SessionsManagerClientError> {
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()?,
            api: ControlPlaneApi::new(validate_base_url(base_url)?),
            credentials: Arc::new(NoCredentials),
        })
    }

    /// Reaches the sessions-manager at `base_url` as an agent, with the shared secret and the API
    /// key the environment configures, if any.
    pub fn from_env(base_url: Url) -> Result<Self, SessionsManagerClientError> {
        Ok(Self::new(base_url)?.with_credentials(credentials_from_env()?))
    }

    /// Reaches the sessions-manager at `base_url` as an intproxy, with the shared secret the
    /// environment configures, if any, and the developer's login token from `login_tokens`.
    pub fn intproxy_from_env(
        base_url: Url,
        login_tokens: Option<Arc<dyn LoginTokenSource>>,
    ) -> Result<Self, SessionsManagerClientError> {
        let credentials = intproxy_credentials_from_env(login_tokens, &base_url)?;

        Ok(Self::new(base_url)?.with_credentials(credentials))
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Dials and authorizes the data-plane WebSocket described by a control-plane assignment.
    ///
    /// Assignment endpoints are relative to the configured base URL. The assignment
    /// authorization is forwarded only after resolving the endpoint against that trusted origin.
    async fn connect_websocket(
        &self,
        assignment: &ConnectionAssignment,
    ) -> Result<WebSocketStream<TokioIo<Upgraded>>, SessionsManagerClientError> {
        let authorization = data_plane_authorization_headers(assignment)?;
        let base_url = self.api.base_url();
        let scheme = match base_url.scheme() {
            "http" | "ws" => "http",
            "https" | "wss" => "https",
            _ => {
                return Err(SessionsManagerClientError::InvalidBaseUrlScheme(
                    base_url.clone(),
                ));
            }
        };

        let url = assignment.data_plane_endpoint.resolve(base_url, scheme)?;

        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let client: HyperClient<_, Empty<Bytes>> =
            HyperClient::builder(TokioExecutor::new()).build(connector);

        let started_at = Instant::now();
        tokio::time::timeout(WEBSOCKET_UPGRADE_TIMEOUT, async {
            // The credentials are gathered inside the timeout: a provider may have to reach the
            // network for them, and that wait belongs to the upgrade it is holding up.
            let mut request = Request::builder().uri(url.as_str()).body(Vec::new())?;
            request
                .headers_mut()
                .extend(self.credentials.data_plane_headers().await?);
            request.headers_mut().extend(authorization);

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
}

impl SessionsManagerTransport for DirectTransport {
    async fn subscribe_assignments(
        &self,
        scope: &ServiceScope,
        subscription: &AssignmentSubscription,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        let deadline = Instant::now() + RESPONSE_HEADER_TIMEOUT;
        let endpoint = self
            .api
            .endpoint(ControlPlaneEndpoint::Assignments { scope })?;
        tracing::debug!(
            %endpoint,
            subscription = ?subscription,
            "requesting sessions-manager assignments"
        );
        // The whole attempt, credentials included, shares one deadline: a provider that has to
        // fetch something before it can answer is part of what makes subscribing slow.
        let credentials =
            with_deadline(Some(deadline), self.credentials.control_plane_headers()).await??;
        let sent_authorization = credentials.contains_key(AUTHORIZATION);
        let request = self
            .http
            .get(endpoint)
            .query(subscription)
            .headers(credentials)
            .header(ACCEPT, "text/event-stream")
            .send();
        let response = with_deadline(Some(deadline), request).await??;
        if response.status() == StatusCode::UNAUTHORIZED
            && sent_authorization
            && let Some(error) = self.credentials.unauthorized_error()
        {
            return Err(error);
        }
        verify_event_stream(response.status(), response.headers())?;

        Ok(ControlPlaneEventStream::from_bytes(response.bytes_stream()))
    }

    async fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
        &self,
        assignment: ConnectionAssignment,
    ) -> Result<OperatorConnection<E>, SessionsManagerClientError> {
        Ok(OperatorConnection::new(
            self.connect_websocket(&assignment).await?,
        ))
    }
}

/// Headers that authorize the data-plane upgrade for `assignment`.
///
/// A standalone sessions-manager validates the assignment's one-use credential from the standard
/// `Authorization` header. [`CredentialProvider`] headers use their own names, so both can travel
/// on the same upgrade request.
fn data_plane_authorization_headers(
    assignment: &ConnectionAssignment,
) -> Result<HeaderMap, SessionsManagerClientError> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, assignment_authorization(assignment)?);
    Ok(headers)
}

/// Accepts only hierarchical HTTP(S) URLs without query or fragment, and normalizes the path to
/// end in `/` so endpoint segments are appended rather than replacing the last one.
fn validate_base_url(mut base_url: Url) -> Result<Url, SessionsManagerClientError> {
    if !matches!(base_url.scheme(), "http" | "https")
        || base_url.cannot_be_a_base()
        || base_url.host().is_none()
        || base_url.query().is_some()
        || base_url.fragment().is_some()
    {
        return Err(SessionsManagerClientError::InvalidBaseUrl);
    }

    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }

    Ok(base_url)
}

#[cfg(test)]
mod tests {
    use axum::{Router, response::IntoResponse};
    use futures::future::{BoxFuture, FutureExt};
    use mirrord_sessions_manager_protocol::ServiceScope;
    use tokio::net::TcpListener;

    use super::*;
    use crate::{IntproxyClient, LoginToken, LoginTokenCredentials, SessionsManagerConnectInfo};

    struct NoLogin;

    impl LoginTokenSource for NoLogin {
        fn login_token(
            &self,
        ) -> BoxFuture<'_, Result<Option<LoginToken>, SessionsManagerClientError>> {
            std::future::ready(Ok(None)).boxed()
        }
    }

    /// No login token was sent, so `mirrord login` is not what refused the session, and the 401
    /// is reported as is.
    #[tokio::test]
    async fn a_401_without_a_login_token_is_reported_as_the_bare_status() {
        let app = Router::new().fallback(|| async { StatusCode::UNAUTHORIZED.into_response() });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url =
            Url::parse(&format!("http://{}/sm", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let transport = DirectTransport::new(base_url.clone())
            .unwrap()
            .with_credentials(Arc::new(LoginTokenCredentials::new(
                Arc::new(NoLogin),
                &base_url,
            )));
        let client = IntproxyClient::with_transport(
            SessionsManagerConnectInfo {
                scope: ServiceScope {
                    environment: "staging".to_owned(),
                    service: "api".to_owned(),
                },
                user_session_id: "session".to_owned(),
                replica_filter: None,
            },
            transport,
        )
        .unwrap();

        let error = client.connect(Duration::from_secs(5)).await.unwrap_err();

        assert!(
            matches!(
                error,
                SessionsManagerClientError::HttpStatus(StatusCode::UNAUTHORIZED)
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn explicit_url_is_used_as_set() {
        let endpoint = SessionsManagerEndpoint::resolve(Some("https://sm.example.com/sm")).unwrap();

        assert_eq!(endpoint.url.as_str(), "https://sm.example.com/sm");
        assert_eq!(endpoint.source, EndpointSource::Explicit);
    }

    #[test]
    fn metalbear_cloud_is_the_default() {
        let endpoint = SessionsManagerEndpoint::resolve(None).unwrap();

        assert_eq!(
            endpoint.url.as_str(),
            "https://sm-aws-use1.app.metalbear.com/sm"
        );
        assert_eq!(endpoint.source, EndpointSource::Default);
        DirectTransport::new(endpoint.url).expect("the default must be a valid base URL");
    }

    /// An explicit URL that doesn't parse is an error, not a reason to fall back to MetalBear
    /// Cloud.
    #[test]
    fn invalid_explicit_url_is_an_error() {
        assert!(matches!(
            SessionsManagerEndpoint::resolve(Some("not a url")),
            Err(SessionsManagerClientError::Url(_))
        ));
    }
}
