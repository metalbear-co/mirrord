use serde::Deserialize;

use crate::config::{ConfigError, MirrordConfig};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum IncomingField {
    Mirror,
    Steal,
}

impl IncomingField {
    pub fn is_steal(&self) -> bool {
        self == &IncomingField::Steal
    }
}

impl MirrordConfig for Option<IncomingField> {
    type Generated = IncomingField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let env_value = std::env::var("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
            .ok()
            .and_then(|val| val.parse::<bool>().ok())
            .map(|val| {
                if val {
                    IncomingField::Steal
                } else {
                    IncomingField::Mirror
                }
            });

        Ok(env_value.or(self).unwrap_or(IncomingField::Mirror))
    }
}
