use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    ops::Not,
};

use mirrord_analytics::AnalyticsReporter;
use mirrord_config::LayerConfig;
use tempfile::NamedTempFile;

use super::{error::ComposeError, ComposeResult, MIRRORD_COMPOSE_SIDECAR_SERVICE};
use crate::{
    container::command_builder::RuntimeCommandBuilder, execution::LINUX_INJECTION_ENV_VAR,
    MirrordExecution,
};

#[derive(Debug, Default)]
pub(crate) struct ContainerInfo {
    pub(crate) env_vars: HashMap<String, String>,
    pub(crate) volumes: HashMap<String, String>,
    volumes_from: HashSet<String>,
    networks: HashSet<String>,
    extra_args: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct New;

#[derive(Debug)]
pub(crate) struct PrepareConfigAndAnalytics;

#[derive(Debug)]
pub(crate) struct PrepareTLS {
    pub(super) config: LayerConfig,
    pub(super) analytics: AnalyticsReporter,
}

#[derive(Debug)]
pub(crate) struct PrepareLayerConfig {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
}

#[derive(Debug)]
pub(crate) struct PrepareExternalProxy {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
}

#[derive(Debug)]
pub(crate) struct PrepareRuntimeCommand {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) external_proxy: MirrordExecution,
}

#[derive(Debug)]
pub(crate) struct PrepareCompose {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) runtime_command_builder: RuntimeCommandBuilder,
}

#[derive(Debug)]
pub(crate) struct RunCompose {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) runtime_command_builder: RuntimeCommandBuilder,
    pub(super) compose_yaml: NamedTempFile,
}

#[derive(Debug)]
pub(super) struct ComposeYamler<'a> {
    pub(super) service: &'a mut serde_yaml::Mapping,
}

impl<'a> ComposeYamler<'a> {
    // TODO(alex) [mid]: Can still be improved, the stepification.
    pub(super) fn prepare_yaml<const IS_MIRRORD: bool>(
        service: &'a mut serde_yaml::Mapping,
        command: &RuntimeCommandBuilder,
    ) -> ComposeResult<()> {
        let mut yamler = Self { service };

        yamler.prepare_volumes(command)?;
        yamler.prepare_volumes_from(IS_MIRRORD)?;
        yamler.prepare_env_vars(command, IS_MIRRORD)?;
        yamler.prepare_network(IS_MIRRORD)?;
        yamler.prepare_depends_on(IS_MIRRORD)?;

        Ok(())
    }

    fn prepare_volumes(&mut self, command: &RuntimeCommandBuilder) -> ComposeResult<()> {
        for (mirrord_volume_key, mirrord_volume) in command.volumes.iter() {
            match self
                .service
                .get_mut("volumes")
                .and_then(|volume| volume.as_sequence_mut())
            {
                Some(volume) => {
                    volume.push(serde_yaml::from_str(&format!(
                        "{mirrord_volume_key}:{mirrord_volume}"
                    ))?);
                }
                None => {
                    self.service.insert(
                        serde_yaml::from_str("volumes")?,
                        serde_yaml::from_str(&format!("- {mirrord_volume_key}:{mirrord_volume}"))?,
                    );
                }
            };
        }

        Ok(())
    }

    fn prepare_volumes_from(&mut self, is_mirrord: bool) -> ComposeResult<()> {
        if is_mirrord.not() {
            match self
                .service
                .get_mut("volumes_from")
                .and_then(|volumes_from| volumes_from.as_sequence_mut())
            {
                Some(volumes_from) => volumes_from.push(serde_yaml::from_str("mirrord-sidecar")?),
                None => {
                    self.service.insert(
                        serde_yaml::from_str("volumes_from")?,
                        serde_yaml::from_str("- mirrord-sidecar")?,
                    );
                }
            };
        }

        Ok(())
    }

    fn prepare_env_vars(
        &mut self,
        command: &RuntimeCommandBuilder,
        is_mirrord: bool,
    ) -> ComposeResult<()> {
        for (mirrord_env_key, mirrord_env) in command.env_vars.iter() {
            if mirrord_env_key.contains("LOCALSTACK") {
                continue;
            }

            if is_mirrord && mirrord_env_key.contains(LINUX_INJECTION_ENV_VAR) {
                continue;
            }

            match self
                .service
                .get_mut("environment")
                .and_then(|env| env.as_mapping_mut())
            {
                Some(env) => {
                    env.insert(
                        serde_yaml::from_str(&format!("{mirrord_env_key}"))?,
                        serde_yaml::from_str(&format!("{mirrord_env}"))?,
                    );
                }
                None => {
                    self.service.insert(
                        serde_yaml::from_str("environment")?,
                        serde_yaml::from_str(&format!(r#"{mirrord_env_key}: "{mirrord_env}""#))?,
                    );
                }
            }
        }

        Ok(())
    }

    fn prepare_network(&mut self, is_mirrord: bool) -> ComposeResult<()> {
        if is_mirrord.not() {
            self.service.insert(
                serde_yaml::from_str("network_mode")?,
                serde_yaml::from_str("service:mirrord-sidecar")?,
            );
            self.service.remove("networks");
        }

        Ok(())
    }

    fn prepare_depends_on(&mut self, is_mirrord: bool) -> ComposeResult<()> {
        if is_mirrord.not() {
            match self
                .service
                .get_mut("depends_on")
                .and_then(|depends_on| depends_on.as_sequence_mut())
            {
                Some(depends_on) => depends_on.push(serde_yaml::from_str("mirrord-sidecar")?),
                None => {
                    self.service.insert(
                        serde_yaml::from_str("depends_on")?,
                        serde_yaml::from_str("- mirrord-sidecar")?,
                    );
                }
            };
        }
        Ok(())
    }
}
