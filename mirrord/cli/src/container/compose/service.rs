use std::collections::{HashMap, HashSet};

use super::ComposeResult;

// TODO(alex) [high]: How do I improve this mess so that it's more builder like, and I can
// remove stuff from it to insert in sidecar?
//
// I think this should be something that gets parsed from the user services, and I just add
// to it (mirrord env vars, and other env vars that come from my system). So I should read the
// user compose BEFORE adding stuff to it? Seems to make sense, read it and add it to here,
// then remove conflicting stuff, then rebuild the user services with what we have here.
//
// This means that this will also have things like `name: redis`, `command: foo -t`, and so on.
// Be more of a `compose.yaml` parser (or don't? since we only want stuff that's relevant to us to
// be in here).
#[derive(Debug, Default, Clone)]
pub(super) struct ServiceInfo {
    pub(super) env_vars: HashMap<String, String>,
    pub(super) volumes: HashMap<String, String>,
}

impl ServiceInfo {
    pub(super) fn prepare_yaml(self, service: &mut serde_yaml::Mapping) -> ComposeResult<()> {
        let Self { env_vars, volumes } = self;

        for (mirrord_volume_key, mirrord_volume) in volumes.iter() {
            match service
                .get_mut("volumes")
                .and_then(|volume| volume.as_sequence_mut())
            {
                Some(volume) => {
                    volume.push(serde_yaml::from_str(&format!(
                        "{mirrord_volume_key}:{mirrord_volume}"
                    ))?);
                }
                None => {
                    service.insert(
                        serde_yaml::from_str("volumes")?,
                        serde_yaml::from_str(&format!("- {mirrord_volume_key}:{mirrord_volume}"))?,
                    );
                }
            };
        }

        // for mirrord_volume_from in volumes_from.iter() {
        //     match service
        //         .get_mut("volumes_from")
        //         .and_then(|volumes_from| volumes_from.as_sequence_mut())
        //     {
        //         Some(user_volumes_from) => {
        //             user_volumes_from.push(serde_yaml::from_str(mirrord_volume_from)?);
        //             // .push(serde_yaml::from_str("mirrord-sidecar")?)
        //         }
        //         None => {
        //             service.insert(
        //                 serde_yaml::from_str("volumes_from")?,
        //                 serde_yaml::from_str(&format!("- {mirrord_volume_from}"))?,
        //             );
        //         }
        //     };
        // }

        for (mirrord_env_key, mirrord_env) in env_vars.iter() {
            if mirrord_env_key.contains("LOCALSTACK") {
                continue;
            }

            match service
                .get_mut("environment")
                .and_then(|env| env.as_mapping_mut())
            {
                Some(env) => {
                    if mirrord_env_key.contains("MIRRORD_AGENT_CONNECT_INFO") {
                        let v = serde_yaml::Value::String(mirrord_env.clone());
                        env.insert(serde_yaml::from_str(&format!("{mirrord_env_key}"))?, v);
                    } else {
                        env.insert(
                            serde_yaml::from_str(&format!("{mirrord_env_key}"))?,
                            serde_yaml::from_str(&format!("{mirrord_env}"))?,
                        );
                    }
                }
                None => {
                    service.insert(
                        serde_yaml::from_str("environment")?,
                        serde_yaml::from_str(&format!(r#"{mirrord_env_key}: "{mirrord_env}""#))?,
                    );
                }
            }
        }

        // if let Some(_) = network_mode {
        //     service.insert(
        //         serde_yaml::from_str("network_mode")?,
        //         serde_yaml::from_str("service:mirrord-sidecar")?,
        //     );
        //     service.remove("networks");
        // }

        // if let Some(_) = depends_on {
        //     match service
        //         .get_mut("depends_on")
        //         .and_then(|depends_on| depends_on.as_sequence_mut())
        //     {
        //         Some(depends_on) => depends_on.push(serde_yaml::from_str("mirrord-sidecar")?),
        //         None => {
        //             service.insert(
        //                 serde_yaml::from_str("depends_on")?,
        //                 serde_yaml::from_str("- mirrord-sidecar")?,
        //             );
        //         }
        //     };
        // }

        Ok(())
    }
}
