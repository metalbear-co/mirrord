//! A standalone sessions-manager reached at its own base URL.

use std::{env::VarError, sync::Arc, time::Duration};

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
use url::{Host, Url};

use super::{SessionsManagerTransport, assignment_authorization};
use crate::{
    control_plane::{
        ControlPlaneEventStream,
        api::{ControlPlaneApi, ControlPlaneEndpoint},
        verify_event_stream,
    },
    credentials::{
        CredentialProvider, LoginTokenSource, NoCredentials, credentials_from_env,
        intproxy_credentials_from_env,
    },
    error::SessionsManagerClientError,
    retry::with_deadline,
};

/// Complete sessions-manager API base URL, including any deployment-specific path prefix.
///
/// For example, both `https://example.com/sm` and `https://sm.example.com` are valid; API version
/// and resource segments are appended to the configured path.
pub const SESSIONS_MANAGER_URL_ENV: &str = "MIRRORD_SESSIONS_MANAGER_URL";

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

    /// Reaches the sessions-manager at [`SESSIONS_MANAGER_URL_ENV`] as an agent, with the shared
    /// secret and the API key the environment configures, if any.
    pub fn from_env() -> Result<Self, SessionsManagerClientError> {
        Ok(Self::new(base_url_from_env()?)?.with_credentials(credentials_from_env()?))
    }

    /// Reaches the sessions-manager at [`SESSIONS_MANAGER_URL_ENV`] as an intproxy, with the
    /// shared secret the environment configures, if any, and the developer's login token from
    /// `login_tokens`.
    ///
    /// The login token identifies the developer for as long as it lasts, so it goes only over TLS,
    /// or over plain HTTP to a sessions-manager on this machine, as when it is run locally or
    /// reached through a port-forward.
    pub fn intproxy_from_env(
        login_tokens: Option<Arc<dyn LoginTokenSource>>,
    ) -> Result<Self, SessionsManagerClientError> {
        let base_url = base_url_from_env()?;
        if login_tokens.is_some() && base_url.scheme() != "https" && !is_loopback(&base_url) {
            return Err(SessionsManagerClientError::InsecureSessionsManagerUrl(
                base_url,
            ));
        }

        Ok(Self::new(base_url)?.with_credentials(intproxy_credentials_from_env(login_tokens)?))
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
        let request = self
            .http
            .get(endpoint)
            .query(subscription)
            .headers(credentials)
            .header(ACCEPT, "text/event-stream")
            .send();
        let response = with_deadline(Some(deadline), request).await??;
        if response.status() == StatusCode::UNAUTHORIZED
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

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Reads the sessions-manager base URL from [`SESSIONS_MANAGER_URL_ENV`].
fn base_url_from_env() -> Result<Url, SessionsManagerClientError> {
    match std::env::var(SESSIONS_MANAGER_URL_ENV) {
        Ok(raw) => Ok(Url::parse(&raw)?),
        Err(VarError::NotPresent) => Err(SessionsManagerClientError::MissingSessionsManagerUrl),
        Err(error) => Err(error.into()),
    }
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
