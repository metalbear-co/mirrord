use std::{
    fs::File,
    io::{BufReader, Write},
    net::SocketAddr,
    ops::Not,
    path::{Path, PathBuf},
    process::Stdio,
};

use error::ComposeError;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, Reporter};
use mirrord_config::{
    external_proxy::{MIRRORD_EXTERNAL_TLS_CERTIFICATE_ENV, MIRRORD_EXTERNAL_TLS_KEY_ENV},
    internal_proxy::{
        MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE_ENV, MIRRORD_INTPROXY_CLIENT_TLS_KEY_ENV,
    },
    LayerConfig, MIRRORD_CONFIG_FILE_ENV,
};
use mirrord_progress::{Progress, ProgressTracker, MIRRORD_PROGRESS_ENV};
use steps::*;
use tempfile::NamedTempFile;
use thiserror::Error;
use tokio::process::Command;
use tracing::Level;

use super::{
    command_builder::RuntimeCommandBuilder, create_temp_layer_config,
    prepare_tls_certs_for_container,
};
use crate::{
    config::RuntimeArgs,
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::{create_config_and_analytics, CONTAINER_EXECUTION_KIND},
    error::ContainerError,
    execution::{LINUX_INJECTION_ENV_VAR, MIRRORD_CONNECT_TCP_ENV, MIRRORD_EXECUTION_KIND_ENV},
    util::MIRRORD_CONSOLE_ADDR_ENV,
    CliError, ContainerRuntime, ExecParams, MirrordExecution,
};

type ComposeResult<T> = Result<T, ComposeError>;

mod error;
mod steps;

#[derive(Debug)]
pub(super) struct RunCompose {
    internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    analytics: AnalyticsReporter,
    config: LayerConfig,
    layer_config_file: NamedTempFile,
    runtime_command_builder: RuntimeCommandBuilder,
    compose_yaml: NamedTempFile,
}

#[derive(Debug)]
pub(super) struct ComposeRunner<Step>
where
    Step: core::fmt::Debug,
{
    progress: ProgressTracker,
    runtime: ContainerRuntime,
    runtime_args: Vec<String>,
    step: Step,
}

impl ComposeRunner<New> {
    #[tracing::instrument(level = Level::DEBUG, err)]
    pub(super) fn try_new(
        runtime_args: RuntimeArgs,
        exec_params: ExecParams,
    ) -> ComposeResult<ComposeRunner<PrepareConfigAndAnalytics>> {
        let RuntimeArgs { runtime, command } = runtime_args;

        let progress = ProgressTracker::from_env("mirrord container");

        if command.has_publish() {
            progress.warning("mirrord container may have problems with \"-p\" when used as part of container run command, please add the publish arguments to \"contanier.cli_extra_args\" in config if you are planning to publish ports");
        }

        progress.warning("mirrord container is currently an unstable feature");

        for (name, value) in exec_params.as_env_vars()? {
            std::env::set_var(name, value);
        }

        std::env::set_var(
            MIRRORD_EXECUTION_KIND_ENV,
            (CONTAINER_EXECUTION_KIND as u32).to_string(),
        );

        Ok(ComposeRunner {
            progress,
            step: PrepareConfigAndAnalytics,
            runtime,
            runtime_args: vec![vec!["compose".to_string()], command.into_parts().1].concat(),
        })
    }
}

impl ComposeRunner<PrepareConfigAndAnalytics> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) fn prepare_config_and_analytics(
        self,
        watch: drain::Watch,
    ) -> ComposeResult<ComposeRunner<PrepareTLS>> {
        let Self {
            mut progress,
            runtime,
            runtime_args,
            step: _,
        } = self;

        let (config, analytics) = create_config_and_analytics(&mut progress, watch)?;

        Ok(ComposeRunner {
            progress,
            step: PrepareTLS { config, analytics },
            runtime,
            runtime_args,
        })
    }
}

impl ComposeRunner<PrepareTLS> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) fn prepare_tls(self) -> ComposeResult<ComposeRunner<PrepareLayerConfig>> {
        let Self {
            progress,
            step: PrepareTLS {
                mut config,
                analytics,
            },
            runtime,
            runtime_args,
        } = self;

        let (internal_proxy_tls_guards, external_proxy_tls_guards) =
            prepare_tls_certs_for_container(&mut config)?;

        Ok(ComposeRunner {
            progress,
            step: PrepareLayerConfig {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                config,
            },
            runtime,
            runtime_args,
        })
    }
}

impl ComposeRunner<PrepareLayerConfig> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) fn prepare_layer_config(self) -> ComposeResult<ComposeRunner<PrepareExternalProxy>> {
        let Self {
            progress,
            step:
                PrepareLayerConfig {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    analytics,
                    config,
                },
            runtime,
            runtime_args,
        } = self;

        let layer_config_file = create_temp_layer_config(&config)?;
        std::env::set_var(MIRRORD_CONFIG_FILE_ENV, layer_config_file.path());

        // TODO(alex) [high]: Prepare the sidecar command, the `compose.yaml` with `depends_on`
        // added to the user's services, and then `docker compose run`?
        Ok(ComposeRunner {
            progress,
            step: PrepareExternalProxy {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                config,
                layer_config_file,
            },
            runtime,
            runtime_args,
        })
    }
}

impl ComposeRunner<PrepareExternalProxy> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn start_external_proxy(self) -> ComposeResult<ComposeRunner<PrepareCompose>> {
        let Self {
            progress,
            step:
                PrepareExternalProxy {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    mut analytics,
                    config,
                    layer_config_file,
                },
            runtime,
            runtime_args,
        } = self;

        let mut external_proxy_progress = progress.subtask("preparing external proxy");
        let external_proxy_execution =
            MirrordExecution::start_external(&config, &mut external_proxy_progress, &mut analytics)
                .await?;

        let mut connection_info = Vec::new();
        let mut execution_info_env_without_connection_info = Vec::new();
        for (key, value) in &external_proxy_execution.environment {
            if key == MIRRORD_CONNECT_TCP_ENV || key == AGENT_CONNECT_INFO_ENV_KEY {
                connection_info.push((key.as_str(), value.as_str()));
            } else {
                execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
            }
        }

        external_proxy_progress.success(None);

        let mut runtime_command_builder = RuntimeCommandBuilder::new(runtime);

        if let Ok(console_addr) = std::env::var(MIRRORD_CONSOLE_ADDR_ENV) {
            if console_addr
                .parse()
                .map(|addr: SocketAddr| !addr.ip().is_loopback())
                .unwrap_or_default()
            {
                runtime_command_builder.add_env(MIRRORD_CONSOLE_ADDR_ENV, console_addr);
            } else {
                // TODO(alex) [mid]: Use eth0 ip.
                tracing::warn!(
                ?console_addr,
                "{MIRRORD_CONSOLE_ADDR_ENV} needs to be a non loopback address when used with containers"
            );
            }
        }

        runtime_command_builder.add_env(MIRRORD_PROGRESS_ENV, "off");
        runtime_command_builder.add_env(
            MIRRORD_EXECUTION_KIND_ENV,
            (CONTAINER_EXECUTION_KIND as u32).to_string(),
        );

        runtime_command_builder.add_env(MIRRORD_CONFIG_FILE_ENV, "/tmp/mirrord-config.json");
        runtime_command_builder
            .add_volume::<true, _, _>(layer_config_file.path(), "/tmp/mirrord-config.json");

        let mut load_env_and_mount_pem = |env: &str, path: &Path| {
            let container_path = format!("/tmp/{}.pem", env.to_lowercase());

            runtime_command_builder.add_env(env, &container_path);
            runtime_command_builder.add_volume::<true, _, _>(path, container_path);
        };

        if let Some(path) = config.internal_proxy.client_tls_certificate.as_ref() {
            load_env_and_mount_pem(MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE_ENV, path)
        }

        if let Some(path) = config.internal_proxy.client_tls_key.as_ref() {
            load_env_and_mount_pem(MIRRORD_INTPROXY_CLIENT_TLS_KEY_ENV, path)
        }

        if let Some(path) = config.external_proxy.tls_certificate.as_ref() {
            load_env_and_mount_pem(MIRRORD_EXTERNAL_TLS_CERTIFICATE_ENV, path)
        }

        if let Some(path) = config.external_proxy.tls_key.as_ref() {
            load_env_and_mount_pem(MIRRORD_EXTERNAL_TLS_KEY_ENV, path)
        }

        runtime_command_builder.add_envs(execution_info_env_without_connection_info);

        runtime_command_builder.add_env(
            LINUX_INJECTION_ENV_VAR,
            config.container.cli_image_lib_path.clone(),
        );

        // TODO(alex) [high]: Prepare the sidecar command, the `compose.yaml` with `depends_on`
        // added to the user's services, and then `docker compose run`?
        Ok(ComposeRunner {
            progress,
            step: PrepareCompose {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                config,
                layer_config_file,
                runtime_command_builder,
            },
            runtime,
            runtime_args,
        })
    }
}

const MIRRORD_COMPOSE_SIDECAR_SERVICE: &str = "mirrord-sidecar";

impl ComposeRunner<PrepareCompose> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn prepare_compose_file(self) -> ComposeResult<ComposeRunner<RunCompose>> {
        // TODO(alex) [high]: Prepare the compose file, read `services` adding our mirrord proxy,
        // and modify user's with `depends_on`.
        let mut user_compose = self.read_user_compose_file()?;

        let Self {
            progress,
            step:
                PrepareCompose {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    analytics,
                    config,
                    layer_config_file,
                    runtime_command_builder,
                },
            runtime,
            runtime_args,
        } = self;

        let services = Self::prepare_services(&mut user_compose)?;

        for (service_key, service) in services.iter_mut() {
            if service_key
                .as_str()
                .is_some_and(|key| key == MIRRORD_COMPOSE_SIDECAR_SERVICE)
            {
                continue;
            }

            let service = service
                .as_mapping_mut()
                .ok_or_else(|| ComposeError::UnexpectedType("mapping".to_string()))?;

            ComposeYamler::prepare_yaml(service, &runtime_command_builder)?;
        }

        let mut compose_yaml = tempfile::Builder::new()
            .prefix("mirrord-compose")
            .suffix(".yaml")
            .tempfile()
            .map_err(ContainerError::ConfigWrite)?;

        compose_yaml.write_all(serde_yaml::to_string(&user_compose)?.as_bytes())?;

        Ok(ComposeRunner {
            progress,
            step: RunCompose {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                config,
                layer_config_file,
                runtime_command_builder,
                compose_yaml,
            },
            runtime,
            runtime_args,
        })
    }

    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    fn read_user_compose_file(&self) -> ComposeResult<serde_yaml::Value> {
        self.progress.info("Preparing mirrord `compose.yaml`.");

        let compose_file_path = self
            .runtime_args
            .iter()
            .inspect(|arg| tracing::debug!(?arg))
            .skip_while(|arg| arg.contains("--file").not() || arg.contains("-f").not())
            .next()
            .map(|compose_file_path| PathBuf::from(&compose_file_path))
            .unwrap_or_else(|| PathBuf::from("./compose.yaml"));

        tracing::debug!(?compose_file_path);

        let user_compose_file = File::open(compose_file_path)?;
        let reader = BufReader::new(user_compose_file);
        let user_compose: serde_yaml::Value = serde_yaml::from_reader(reader)?;

        Ok(user_compose)
    }

    fn prepare_services(
        compose: &mut serde_yaml::Value,
    ) -> ComposeResult<&mut serde_yaml::Mapping> {
        let services = compose
            .get_mut("services")
            .ok_or_else(|| ComposeError::MissingField("services".to_string()))?
            .as_mapping_mut()
            .ok_or_else(|| ComposeError::UnexpectedType("mapping".to_string()))?;

        services.insert(
            serde_yaml::from_str(MIRRORD_COMPOSE_SIDECAR_SERVICE)?,
            serde_yaml::from_str(&format!(
                r#"
                        image: meowjesty/mirrord-cli:latest
                        ports:
                          - "8000:5000"
                    "#
            ))?,
        );

        Ok(services)
    }
}

impl ComposeRunner<RunCompose> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn debug_stuff(self) -> ComposeResult<ComposeRunner<()>> {
        let Self {
            progress,
            runtime,
            mut runtime_args,
            step:
                RunCompose {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    mut analytics,
                    config,
                    layer_config_file,
                    runtime_command_builder,
                    compose_yaml,
                },
        } = self;

        // TODO(alex) [low]: Don't keep the file forever here, it should be kept only during
        // execution.
        let (p, path) = compose_yaml.keep().unwrap();

        // TODO(alex) [high]: To connect to mirrord, we can add this address directly in the
        // compose file, in `mirrord-sidecar` as an env, and the other services too.
        // Is it random though? If it is, then maybe refactor it into a config so it stops being
        // random.
        //
        // Must also start the external proxy!
        //
        // runtime_command.add_env(
        //     MIRRORD_CONNECT_TCP_ENV,
        //     sidecar_intproxy_address.to_string(),
        // );
        let mut args = vec![
            "compose".to_string(),
            "--file".to_string(),
            path.to_string_lossy().to_string(),
            "up".to_string(),
            // "run".to_string(),
            // "interactive".to_string(),
            // "/bin/bash".to_string(),
        ];
        // runtime_args.remove(0);
        // args.extend(runtime_args.clone());
        // runtime_args.extend(args);
        tracing::info!(?args, "We're running it");

        let runtime_command_result = Command::new("docker")
            .args(args.clone())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await;

        if let Some((cert, key)) = internal_proxy_tls_guards {
            if let Err(err) = cert.keep() {
                tracing::warn!(?err, "failed to keep internal proxy certificate");
            }

            if let Err(err) = key.keep() {
                tracing::warn!(?err, "failed to keep internal proxy key");
            }
        }

        if let Err(err) = layer_config_file.keep() {
            tracing::warn!(?err, "failed to keep composed config file");
        }

        match runtime_command_result {
            Err(err) => {
                analytics.set_error(AnalyticsError::BinaryExecuteFailed);
            }
            Ok(status) => (),
        };

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: (),
        })
    }
}
