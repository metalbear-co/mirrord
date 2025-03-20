use mirrord_config::feature::network::incoming::ConcurrentSteal;
use serde::Serialize;

/// Query params for the operator connect request.
///
/// Use [`serde_qs`] to get the query string.
#[derive(Serialize)]
pub struct ConnectParams<'a> {
    /// Should always be true.
    pub connect: bool,
    /// Desired behavior on steal subscription conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_concurrent_steal: Option<ConcurrentSteal>,
    /// Selected mirrord profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'a str>,
}
