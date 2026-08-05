//! Dialing the operator directly for the session connection, instead of being proxied by the
//! Kubernetes API server.
//!
//! The API server path is still what sets a session up, and it is what makes this one possible:
//! the ticket and the operator certificate both come back over a request the API server has
//! already authenticated. See [`mirrord_quic::session`] for what that buys and why it is safe.
//!
//! Everything here is best-effort. A failure at any point means the session runs on the websocket
//! instead, which is why no error in this module reaches the user as a failure.

use std::{net::SocketAddr, time::Duration};

use base64::{Engine, engine::general_purpose};
use http::Request;
use kube::{Client, Resource};
use mirrord_quic::session::{self, SessionStream};
use thiserror::Error;
use tracing::Level;

use crate::{
    crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME, SessionEndpoint},
    types::{SESSION_ID_HEADER, SESSION_TICKET_SUBRESOURCE, SessionRequest, SessionTicket},
};

/// How long the whole dial is given, from the first UDP packet to the operator's verdict.
///
/// An operator whose advertised address is unreachable - a stale address, a firewall dropping UDP,
/// a load balancer that never came up - gives no error, it simply never answers. Without a bound
/// here that would stall every session start by QUIC's idle timeout before falling back, turning a
/// misconfiguration into a permanent tax on session startup.
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// A session that can be carried on a direct connection to the operator.
///
/// Held on the [`OperatorSession`](super::OperatorSession) so that reconnects dial the same way the
/// first connection did.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct DirectSession {
    endpoint: SessionEndpoint,
    namespace: String,
    target: String,
    connect_params: String,
}

impl DirectSession {
    /// Picks apart the connect URL the API server path would have used.
    ///
    /// Deriving the parts from that URL rather than collecting them separately keeps the two paths
    /// from drifting: whatever the connect URL asks for is what the direct connection asks for.
    ///
    /// Returns `None` for anything that is not a plain target - a copied target, most notably -
    /// since only targets are served over the direct path. Those sessions stay on the websocket.
    pub(super) fn new(endpoint: SessionEndpoint, connect_url: &str) -> Option<Self> {
        let (path, connect_params) = connect_url.split_once('?')?;

        let mut segments = path.rsplit('/');
        let target = segments.next()?;
        segments.next().filter(|it| *it == "targets")?;
        let namespace = segments.next()?;
        segments.next().filter(|it| *it == "namespaces")?;

        Some(Self {
            endpoint,
            namespace: namespace.to_owned(),
            target: target.to_owned(),
            connect_params: connect_params.to_owned(),
        })
    }

    /// Asks the operator for a ticket, then redeems it on a fresh QUIC connection.
    ///
    /// The ticket is single use, so this runs in full for every connection, including reconnects.
    #[tracing::instrument(level = Level::TRACE, skip(client), err(level = Level::DEBUG))]
    pub(super) async fn connect(
        &self,
        client: &Client,
        session_id: u64,
    ) -> Result<SessionStream, DirectConnectionError> {
        let ticket = self.request_ticket(client, session_id).await?;

        let certificate = general_purpose::STANDARD
            .decode(&ticket.certificate)
            .map_err(DirectConnectionError::MalformedCertificate)?;
        let config = session::client_config(certificate)?;

        let request = serde_json::to_vec(&SessionRequest {
            ticket: ticket.ticket,
            namespace: self.namespace.clone(),
            target: self.target.clone(),
            connect_params: self.connect_params.clone(),
        })
        .map_err(DirectConnectionError::EncodeRequest)?;

        let address = resolve(&ticket.address).await?;

        tokio::time::timeout(DIAL_TIMEOUT, session::connect(config, address, &request))
            .await
            .map_err(|_| DirectConnectionError::DialTimeout(address))?
            .map_err(DirectConnectionError::from)
    }

    /// Mints a ticket over the API server, which is where the caller's identity is established.
    async fn request_ticket(
        &self,
        client: &Client,
        session_id: u64,
    ) -> Result<SessionTicket, DirectConnectionError> {
        let url_path = MirrordOperatorCrd::url_path(&(), None);
        let request = Request::builder()
            .method(http::Method::POST)
            .uri(format!(
                "{url_path}/{OPERATOR_STATUS_NAME}/{SESSION_TICKET_SUBRESOURCE}"
            ))
            .header(SESSION_ID_HEADER, session_id.to_string())
            .body(Vec::new())
            .map_err(DirectConnectionError::BuildTicketRequest)?;

        client
            .request::<SessionTicket>(request)
            .await
            .map_err(DirectConnectionError::TicketRequestFailed)
    }
}

/// Turns the advertised address into something to send UDP to.
///
/// The address is whatever the installation was configured with, so it may be a name.
async fn resolve(address: &str) -> Result<SocketAddr, DirectConnectionError> {
    tokio::net::lookup_host(address)
        .await
        .map_err(|error| DirectConnectionError::ResolveAddress {
            address: address.to_owned(),
            error,
        })?
        .next()
        .ok_or_else(|| DirectConnectionError::NoAddress(address.to_owned()))
}

/// Why a session could not be carried directly.
///
/// Every one of these is recoverable by falling back to the websocket, so they are logged rather
/// than shown to the user.
#[derive(Debug, Error)]
pub(super) enum DirectConnectionError {
    #[error("failed to build the session ticket request: {0}")]
    BuildTicketRequest(http::Error),
    #[error("the operator did not issue a session ticket: {0}")]
    TicketRequestFailed(kube::Error),
    #[error("the operator's certificate is not valid base64: {0}")]
    MalformedCertificate(base64::DecodeError),
    #[error("failed to encode the session request: {0}")]
    EncodeRequest(serde_json::Error),
    #[error("failed to resolve the operator address `{address}`: {error}")]
    ResolveAddress {
        address: String,
        error: std::io::Error,
    },
    #[error("the operator address `{0}` resolved to nothing")]
    NoAddress(String),
    #[error("failed to set up the QUIC connection: {0}")]
    Setup(#[from] mirrord_quic::QuicSetupError),
    #[error("failed to establish the session stream: {0}")]
    Stream(#[from] mirrord_quic::SessionStreamError),
    #[error("the operator at {0} did not answer within {DIAL_TIMEOUT:?}")]
    DialTimeout(SocketAddr),
}

#[cfg(test)]
mod test {
    use rstest::rstest;

    use super::*;

    fn endpoint() -> SessionEndpoint {
        SessionEndpoint {
            address: "operator.example.com:3000".to_owned(),
        }
    }

    /// Both shapes of target connect URL - direct and through the API server's proxy verb - have to
    /// yield the same namespace and target, since which one the CLI uses depends on the operator's
    /// features rather than on anything about the session.
    #[rstest]
    #[case::url_path(
        "/apis/operator.metalbear.co/v1/namespaces/my-ns/targets/deployment.my-app?connect=true"
    )]
    #[case::proxy(
        "/apis/operator.metalbear.co/v1/proxy/namespaces/my-ns/targets/deployment.my-app?connect=true"
    )]
    fn parses_target_connect_urls(#[case] connect_url: &str) {
        let direct = DirectSession::new(endpoint(), connect_url)
            .expect("a target connect URL should be usable directly");

        assert_eq!(direct.namespace, "my-ns");
        assert_eq!(direct.target, "deployment.my-app");
        assert_eq!(direct.connect_params, "connect=true");
    }

    /// The whole query string has to survive, because the operator parses it with the same code
    /// that parses it off the URL on the API server path.
    #[test]
    fn keeps_the_whole_query_string() {
        let direct = DirectSession::new(
            endpoint(),
            "/apis/operator.metalbear.co/v1/namespaces/ns/targets/pod.p?connect=true&on_concurrent_steal=abort&profile=strict",
        )
        .expect("a target connect URL should be usable directly");

        assert_eq!(
            direct.connect_params,
            "connect=true&on_concurrent_steal=abort&profile=strict",
        );
    }

    /// Only plain targets are served directly. Anything else has to fall back rather than be
    /// mangled into a target request.
    #[rstest]
    #[case::copy_target(
        "/apis/operator.metalbear.co/v1/namespaces/my-ns/copytargets/my-copy?connect=true"
    )]
    #[case::no_query("/apis/operator.metalbear.co/v1/namespaces/my-ns/targets/deployment.my-app")]
    #[case::not_namespaced("/apis/operator.metalbear.co/v1/targets/deployment.my-app?connect=true")]
    fn rejects_everything_else(#[case] connect_url: &str) {
        assert!(
            DirectSession::new(endpoint(), connect_url).is_none(),
            "`{connect_url}` should not be served over a direct connection",
        );
    }
}
