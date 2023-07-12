//! <!--${internal}-->

use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::{MirrordToggleableConfig, VecOrSingle},
};

#[derive(Deserialize, Default, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase")]
pub enum OutgoingFilterConfig {
    #[default]
    Unfiltered,
    Remote(VecOrSingle<String>),
    Local(VecOrSingle<String>),
}

impl MirrordConfig for OutgoingFilterConfig {
    type Generated = Self;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        if let Some(remote) = FromEnv::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_REMOTE")
            .source_value()
            .transpose()?
        {
            Ok(OutgoingFilterConfig::Remote(remote))
        } else if let Some(local) = FromEnv::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_LOCAL")
            .source_value()
            .transpose()?
        {
            Ok(OutgoingFilterConfig::Local(local))
        } else {
            Ok(OutgoingFilterConfig::Unfiltered)
        }
    }
}

impl MirrordToggleableConfig for OutgoingFilterConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        if let Some(remote) = FromEnv::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_REMOTE")
            .source_value()
            .transpose()?
        {
            Ok(OutgoingFilterConfig::Remote(remote))
        } else if let Some(local) = FromEnv::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_LOCAL")
            .source_value()
            .transpose()?
        {
            Ok(OutgoingFilterConfig::Local(local))
        } else {
            Ok(OutgoingFilterConfig::Unfiltered)
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
        let expect = OutgoingFilterConfig::Unfiltered;
        let outgoing_filter_config = OutgoingFilterConfig::default().generate_config().unwrap();

        assert_eq!(outgoing_filter_config, expect);
    }
}
