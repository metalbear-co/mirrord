//! <!--${internal}-->

use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{
        from_env::FromEnv, source::MirrordConfigSource, ConfigError, FromMirrordConfig,
        MirrordConfig,
    },
    util::VecOrSingle,
};

/// List of addresses/ports/subnets that should be sent through either the remote pod or local app,
/// depending how you set this up with either `remote` or `local`.
///
/// You may use this option to specify when outgoing traffic is sent from the remote pod (which
/// is the default behavior when you enable outgoing traffic), or from the local app (default when
/// you have outgoing traffic disabled).
///
/// Takes a list of values, such as:
///
/// - Only UDP traffic on subnet `1.1.1.0/24` on port 1337 will go throuh the remote pod.
///
/// ```json
/// {
///   "remote": ["udp://1.1.1.0/24:1337"]
/// }
///
/// - Only UDP and TCP traffic on resolved address of `google.com` on port `1337` and `7331`
/// will go through the remote pod.
/// ```json
/// {
///   "remote": ["google.com:1337", "google.com:7331"]
/// }
/// ```
/// 
/// - Only TCP traffic on `localhost` on port 1337 will go through the local app, the rest will
///   be emmited remotely in the cluster.
/// ```json
/// {
///   "local": ["tcp://localhost:1337"]
/// }
///
/// - Only outgoing traffic on port `1337` and `7331` will go through the local app.
/// ```json
/// {
///   "local": [":1337", ":7331"]
/// }
/// ```
///
/// Valid values follow this pattern: `[protocol]://[name|address|subnet/mask]:[port]`.
#[derive(Deserialize, Clone, Debug, JsonSchema, Default, PartialEq, Eq)]
pub struct OutgoingFilterConfig(pub OutgoingFilterFileConfig);

impl FromMirrordConfig for OutgoingFilterConfig {
    type Generator = OutgoingFilterFileConfig;
}

/// <--!{$internal}-->
/// The 3 possible variants for the outgoing traffic filter.
#[derive(Deserialize, Default, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutgoingFilterFileConfig {
    /// Will always attempt to go through the remote pod, respecting if outgoing traffic is enabled
    /// or not.
    #[default]
    Unfiltered,

    /// Traffic that matches what's specified here will go through the remote pod, everything else
    /// will go through local.
    Remote(VecOrSingle<String>),

    /// Traffic that matches what's specified here will go through the local app, everything else
    /// will go through the remote pod.
    Local(VecOrSingle<String>),
}

impl MirrordConfig for OutgoingFilterFileConfig {
    type Generated = OutgoingFilterConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        if let Some(remote) =
            FromEnv::<VecOrSingle<String>>::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_REMOTE")
                .source_value()
                .transpose()?
        {
            if remote.is_empty() {
                Err(ConfigError::ValueNotProvided(
                    "outgoing.filter",
                    "remote",
                    None,
                ))
            } else {
                Ok(OutgoingFilterConfig(OutgoingFilterFileConfig::Remote(
                    remote,
                )))
            }
        } else if let Some(local) =
            FromEnv::<VecOrSingle<String>>::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_LOCAL")
                .source_value()
                .transpose()?
        {
            if local.is_empty() {
                Err(ConfigError::ValueNotProvided(
                    "outgoing.filter",
                    "local",
                    None,
                ))
            } else {
                Ok(OutgoingFilterConfig(OutgoingFilterFileConfig::Local(local)))
            }
        } else {
            Ok(OutgoingFilterConfig(OutgoingFilterFileConfig::Unfiltered))
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::config::MirrordConfig;

    #[rstest]
    fn outgoing_filter_config_default() {
        let expect = OutgoingFilterFileConfig::Unfiltered;
        let outgoing_filter_config = OutgoingFilterFileConfig::default()
            .generate_config()
            .unwrap();

        assert_eq!(outgoing_filter_config.0, expect);
    }
}
