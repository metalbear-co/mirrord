use std::fmt;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LicenseInfoOwned {
    pub name: String,
    pub organization: String,
    pub expire_at: NaiveDate,
    /// Fingerprint of the operator license.
    pub fingerprint: Option<String>,
    /// Subscription id encoded in the operator license extension.
    pub subscription_id: Option<String>,
}

/// Name of HTTP header containing CLI version.
/// Sent with each request to the mirrord operator.
pub const MIRRORD_CLI_VERSION_HEADER: &str = "x-mirrord-cli-version";

/// Name of HTTP header containing client certificate.
/// Sent with each request to the mirrord operator (if available) except:
/// 1. Initial GET on the operator resource
/// 2. User certificate request
///
/// Required for making the target connection request.
pub const CLIENT_CERT_HEADER: &str = "x-client-der";

/// Name of HTTP header containing client hostname.
/// Sent with each request to the mirrord operator (if available).
pub const CLIENT_HOSTNAME_HEADER: &str = "x-client-hostname";

/// Name of HTTP header containing client name.
/// Sent with each request to the mirrord operator (if available).
pub const CLIENT_NAME_HEADER: &str = "x-client-name";

/// Name of HTTP header containing operator session id.
/// Sent with target connection request.
pub const SESSION_ID_HEADER: &str = "x-session-id";

/// Name of HTTP header carrying the base64-encoded connect query string.
///
/// The target connection request is a websocket-upgrade `GET`, so its parameters (queue splits,
/// branch databases, profile, etc.) are normally serialized into the URL query string. Some managed
/// ingress proxies (notably GKE Connect Gateway's Envoy) reject query strings containing the
/// percent-encoded JSON we use for complex parameters, failing the upgrade with `400 Bad Request`.
///
/// When the operator advertises [`NewOperatorFeature::ConnectParamsInHeader`], the CLI instead
/// sends the whole connect query string base64-encoded in this header and leaves only
/// `connect=true` in the URL. Header values aren't subject to the same proxy URL validation, so the
/// upgrade succeeds.
///
/// [`NewOperatorFeature::ConnectParamsInHeader`]: crate::crd::NewOperatorFeature::ConnectParamsInHeader
pub const CONNECT_PARAMS_HEADER: &str = "x-mirrord-connect-params";

/// Code returned in error responses from the operator, when reconnecting to a session is no longer
/// possible.
///
/// HTTP 410 Gone.
pub const RECONNECT_NOT_POSSIBLE_CODE: u16 = 410;

/// Reason returned in error responses from the operator, when reconnecting to a session is no
/// longer possible.
pub const RECONNECT_NOT_POSSIBLE_REASON: &str = "ReconnectNotPossible";

/// Kubernetes label key identifying resources owned by the mirrord operator.
pub const OPERATOR_OWNERSHIP_LABEL: &str = "operator.metalbear.co/owner";

/// Name of the environment variable that overrides the default operator isolation marker.
pub const OPERATOR_ISOLATION_MARKER_ENV: &str = "OPERATOR_ISOLATION_MARKER";

/// Default value for the [`OPERATOR_OWNERSHIP_LABEL`] when
/// [`OPERATOR_ISOLATION_MARKER_ENV`] is not set.
pub const DEFAULT_OPERATOR_ISOLATION_MARKER: &str = "mirrord-operator";

/// Label applied to CRDs created during single-cluster sessions on a multi-cluster Primary.
/// The sync controllers check for this label and skip syncing the resource to other clusters,
/// keeping it local to the Primary.
pub const MULTI_CLUSTER_SKIP_SYNC_LABEL: &str = "operator.metalbear.co/skip-mc-sync";

/// Subresource on the operator status resource that mints a [`SessionTicket`].
///
/// `POST /apis/operator.metalbear.co/v1/mirrordoperators/operator/session-ticket`.
pub const SESSION_TICKET_SUBRESOURCE: &str = "session-ticket";

/// Everything the CLI needs to open one session connection directly to the operator, instead of
/// through the Kubernetes API server.
///
/// Minted by the operator in response to a request the API server has already authenticated, which
/// is what lets the QUIC connection carry no identity of its own.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTicket {
    /// Proof that the bearer is the identity this was issued to.
    ///
    /// Single use and short lived, so a leaked ticket buys an attacker one race against the
    /// legitimate CLI rather than a reusable credential.
    pub ticket: String,
    /// Host and port to dial over UDP.
    pub address: String,
    /// The operator's serving certificate, DER, base64 encoded.
    ///
    /// The CLI accepts this certificate and no other on the QUIC connection. It arrives over the
    /// API server, which is what makes it trustworthy.
    pub certificate: String,
    /// How long the ticket stays redeemable, so a CLI that cannot dial in time can say so rather
    /// than reporting a confusing rejection.
    pub expires_in_seconds: u64,
}

impl fmt::Debug for SessionTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTicket")
            .field("ticket", &"<redacted>")
            .field("address", &self.address)
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish_non_exhaustive()
    }
}

/// What the CLI sends first on the QUIC session stream, identifying the session it wants.
///
/// Deliberately says nothing about who the caller is: the operator takes that from the ticket, so
/// there is nothing here worth forging. The target and parameters are not bound to the ticket
/// either, because the operator still checks the caller's Kubernetes permissions against whatever
/// target is asked for.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest {
    /// The `ticket` field of a [`SessionTicket`].
    pub ticket: String,
    /// Namespace of the target to connect to.
    pub namespace: String,
    /// Target to connect to, in the same dotted form the API server path puts in the URL, e.g.
    /// `deployment.my-app.container.web`.
    pub target: String,
    /// Connect parameters, as the same query string the API server path puts in the URL, so that
    /// both paths parse them with the same code.
    pub connect_params: String,
}

impl fmt::Debug for SessionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRequest")
            .field("ticket", &"<redacted>")
            .field("namespace", &self.namespace)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}
