use std::fmt;

use mirrord_config::feature::network::incoming::ConcurrentSteal;
use serde::Serialize;

/// Query params for the operator connect request.
///
/// You can use the [`fmt::Display`] to get a properly encoded query string.
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

impl<'a> fmt::Display for ConnectParams<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_string =
            serde_urlencoded::to_string(self).expect("serialization to memory should not fail");

        f.write_str(&as_string)
    }
}
