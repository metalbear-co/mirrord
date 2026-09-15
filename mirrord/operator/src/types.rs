use std::{borrow::Cow, sync::LazyLock};

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

static OPERATOR_ISOLATION_MARKER: LazyLock<String> = LazyLock::new(|| {
    std::env::var(OPERATOR_ISOLATION_MARKER_ENV)
        .unwrap_or_else(|_| DEFAULT_OPERATOR_ISOLATION_MARKER.to_owned())
});

/// The isolation marker of this process: [`OPERATOR_ISOLATION_MARKER_ENV`] when set, the
/// default otherwise. Read once, so every kind and label in the process agrees on it.
pub fn isolation_marker() -> &'static str {
    &OPERATOR_ISOLATION_MARKER
}

/// Whether this process runs under a marker of its own rather than the default one.
pub fn is_isolated() -> bool {
    isolation_marker() != DEFAULT_OPERATOR_ISOLATION_MARKER
}

/// Set to `true` for an isolated operator to keep its objects under CRDs of its own rather
/// than under the shared ones with an owner label. Off by default: the deployed operator's
/// role has to allow the keyed groups first, and until it does a copy that keyed them would
/// only get Forbidden.
pub const OPERATOR_KEYED_CRDS_ENV: &str = "OPERATOR_KEYED_CRDS";

static OPERATOR_KEYED_CRDS: LazyLock<bool> = LazyLock::new(|| {
    std::env::var(OPERATOR_KEYED_CRDS_ENV).is_ok_and(|value| value == "true" || value == "1")
});

/// Whether this process keeps its objects under CRDs of its own: isolated, and switched on.
pub fn keyed_crds() -> bool {
    is_isolated() && *OPERATOR_KEYED_CRDS
}

/// The root every stored mirrord kind's group ends with; a keyed group keeps it as its
/// suffix so the keyed CRDs of every copy list together under it.
pub const MIRRORD_GROUP_ROOT: &str = "mirrord.metalbear.co";

/// The label between a copy's key and [`MIRRORD_GROUP_ROOT`] in its keyed groups, marking
/// the whole set as test-only CRDs at a glance.
pub const KEYED_GROUP_LABEL: &str = "test";

/// The API group a stored mirrord kind lives under in this process, for
/// `#[kube(group_resolver)]`.
///
/// With the default marker the group is the declared one. An isolated operator, a copy
/// stolen onto a deployed one under a key of its own, gets `<key>.test` in front of the
/// root, `queues.gem.test.mirrord.metalbear.co`, so its objects live in a separate set of
/// CRDs: the deployed operator never sees them, two copies never collide, and each copy's
/// CRDs carry its own schema. Only stored kinds resolve their group; the served
/// `operator.metalbear.co` group is an APIService registration and stays fixed.
pub fn keyed_group(base: &'static str) -> Cow<'static, str> {
    if keyed_crds() {
        keyed_group_for(isolation_marker(), base)
    } else {
        Cow::Borrowed(base)
    }
}

/// The declared group behind a resolved one: what [`keyed_group`] put this process's key
/// into, or the group itself when it carries no key. An isolated copy reads the shared set
/// of CRDs through it. Whether a group is keyed at all is whether this changes it.
pub fn shared_group(group: &str) -> Cow<'_, str> {
    let keyed_root = keyed_root(isolation_marker());
    match group.strip_suffix(keyed_root.as_str()) {
        Some(sub) => Cow::Owned(format!("{sub}{MIRRORD_GROUP_ROOT}")),
        None => Cow::Borrowed(group),
    }
}

/// What replaces [`MIRRORD_GROUP_ROOT`] in the groups of a copy under `marker`.
fn keyed_root(marker: &str) -> String {
    format!("{marker}.{KEYED_GROUP_LABEL}.{MIRRORD_GROUP_ROOT}")
}

#[cfg(test)]
mod shared_group_tests {
    use super::*;

    #[test]
    fn strips_only_this_process_key() {
        // The test process runs with the default marker, so nothing is stripped, and a group
        // carrying another key is left alone.
        assert_eq!(
            shared_group("queues.mirrord.metalbear.co"),
            "queues.mirrord.metalbear.co"
        );
        assert_eq!(
            shared_group("queues.gem.test.mirrord.metalbear.co"),
            "queues.gem.test.mirrord.metalbear.co"
        );
    }
}

/// [`keyed_group`] for a given marker: the declared group under the default marker, the
/// marker and the test label in front of the root otherwise. A group that does not end with
/// the root gets them in front of the whole group, so it is still set apart from the
/// declared one.
pub fn keyed_group_for(marker: &str, base: &'static str) -> Cow<'static, str> {
    if marker == DEFAULT_OPERATOR_ISOLATION_MARKER {
        return Cow::Borrowed(base);
    }
    let keyed_root = keyed_root(marker);
    match base.strip_suffix(MIRRORD_GROUP_ROOT) {
        Some(sub) => Cow::Owned(format!("{sub}{keyed_root}")),
        None => Cow::Owned(format!("{marker}.{KEYED_GROUP_LABEL}.{base}")),
    }
}

#[cfg(test)]
mod keyed_group_tests {
    use super::*;

    #[test]
    fn default_marker_keeps_the_declared_group() {
        assert_eq!(
            keyed_group_for(
                DEFAULT_OPERATOR_ISOLATION_MARKER,
                "queues.mirrord.metalbear.co"
            ),
            "queues.mirrord.metalbear.co"
        );
    }

    #[test]
    fn a_copy_gets_its_key_in_front_of_the_root() {
        assert_eq!(
            keyed_group_for("gem", "queues.mirrord.metalbear.co"),
            "queues.gem.test.mirrord.metalbear.co"
        );
        assert_eq!(
            keyed_group_for("ci-18234", "mirrord.metalbear.co"),
            "ci-18234.test.mirrord.metalbear.co"
        );
    }

    #[test]
    fn a_group_outside_the_root_is_still_set_apart() {
        assert_eq!(
            keyed_group_for("gem", "other.example.com"),
            "gem.test.other.example.com"
        );
    }
}

/// Label applied to CRDs created during single-cluster sessions on a multi-cluster Primary.
/// The sync controllers check for this label and skip syncing the resource to other clusters,
/// keeping it local to the Primary.
pub const MULTI_CLUSTER_SKIP_SYNC_LABEL: &str = "operator.metalbear.co/skip-mc-sync";
