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

#[derive(Deserialize, Clone, Debug, JsonSchema, Default, PartialEq, Eq)]
pub struct OutgoingFilterConfig(pub OutgoingFilterFileConfig);

impl FromMirrordConfig for OutgoingFilterConfig {
    type Generator = OutgoingFilterFileConfig;
}

#[derive(Deserialize, Default, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutgoingFilterFileConfig {
    #[default]
    Unfiltered,
    Remote(VecOrSingle<String>),
    Local(VecOrSingle<String>),
}

impl MirrordConfig for OutgoingFilterFileConfig {
    type Generated = OutgoingFilterConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        if let Some(remote) = FromEnv::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_REMOTE")
            .source_value()
            .transpose()?
        {
            Ok(OutgoingFilterConfig(OutgoingFilterFileConfig::Remote(
                remote,
            )))
        } else if let Some(local) = FromEnv::new("MIRRORD_OUTGOING_TRAFFIC_FILTER_LOCAL")
            .source_value()
            .transpose()?
        {
            Ok(OutgoingFilterConfig(OutgoingFilterFileConfig::Local(local)))
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
