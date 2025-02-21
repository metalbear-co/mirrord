use std::collections::{HashMap, HashSet};

use mirrord_analytics::AnalyticsReporter;
use mirrord_config::LayerConfig;
use tempfile::NamedTempFile;

use super::ComposeResult;
use crate::MirrordExecution;

#[derive(Debug)]
pub(super) struct TlsFileGuard {
    pub(super) cert: NamedTempFile,
    pub(super) key: NamedTempFile,
}

impl From<(NamedTempFile, NamedTempFile)> for TlsFileGuard {
    fn from(value: (NamedTempFile, NamedTempFile)) -> Self {
        Self {
            cert: value.0,
            key: value.1,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct ServiceInfo {
    pub(super) env_vars: HashMap<String, String>,
    pub(super) volumes: HashMap<String, String>,
    pub(super) volumes_from: HashSet<String>,
    pub(super) networks: HashSet<String>,
    pub(super) network_mode: Option<String>,
    pub(super) depends_on: Option<String>,
    pub(super) extra_args: Vec<String>,
}

impl ServiceInfo {
    pub(super) fn prepare_yaml(self, service: &mut serde_yaml::Mapping) -> ComposeResult<()> {
        let Self {
            env_vars,
            volumes,
            volumes_from,
            networks,
            extra_args,
            network_mode,
            depends_on,
        } = self;

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

        for mirrord_volume_from in volumes_from.iter() {
            match service
                .get_mut("volumes_from")
                .and_then(|volumes_from| volumes_from.as_sequence_mut())
            {
                Some(user_volumes_from) => {
                    user_volumes_from.push(serde_yaml::from_str(mirrord_volume_from)?);
                    // .push(serde_yaml::from_str("mirrord-sidecar")?)
                }
                None => {
                    service.insert(
                        serde_yaml::from_str("volumes_from")?,
                        serde_yaml::from_str(&format!("- {mirrord_volume_from}"))?,
                    );
                }
            };
        }

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

        if let Some(_) = network_mode {
            service.insert(
                serde_yaml::from_str("network_mode")?,
                serde_yaml::from_str("service:mirrord-sidecar")?,
            );
            service.remove("networks");
        }

        if let Some(_) = depends_on {
            match service
                .get_mut("depends_on")
                .and_then(|depends_on| depends_on.as_sequence_mut())
            {
                Some(depends_on) => depends_on.push(serde_yaml::from_str("mirrord-sidecar")?),
                None => {
                    service.insert(
                        serde_yaml::from_str("depends_on")?,
                        serde_yaml::from_str("- mirrord-sidecar")?,
                    );
                }
            };
        }

        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct New;

#[derive(Debug)]
pub(crate) struct PrepareConfig;

#[derive(Debug)]
pub(crate) struct PrepareAnalytics {
    pub(super) config: LayerConfig,
}

#[derive(Debug)]
pub(crate) struct PrepareTLS {
    pub(super) config: LayerConfig,
    pub(super) analytics: AnalyticsReporter,
}

#[derive(Debug)]
pub(crate) struct PrepareLayerConfig {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
}

#[derive(Debug)]
pub(crate) struct PrepareExternalProxy {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
}

#[derive(Debug)]
pub(crate) struct PrepareRuntimeCommand {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) external_proxy: MirrordExecution,
}

#[derive(Debug)]
pub(crate) struct PrepareCompose {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) sidecar_info: ServiceInfo,
    pub(super) user_service_info: ServiceInfo,
}

#[derive(Debug)]
pub(crate) struct RunCompose {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) compose_yaml: NamedTempFile,
}
