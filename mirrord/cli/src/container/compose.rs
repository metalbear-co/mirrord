use std::{
    fmt::Debug,
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Not,
    path::{Path, PathBuf},
    process::Stdio,
};

use composer::ServiceComposer;
use error::ComposeError;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, Reporter};
use mirrord_config::{
    external_proxy::{MIRRORD_EXTERNAL_TLS_CERTIFICATE_ENV, MIRRORD_EXTERNAL_TLS_KEY_ENV},
    internal_proxy::{
        MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE_ENV, MIRRORD_INTPROXY_CLIENT_TLS_KEY_ENV,
        MIRRORD_INTPROXY_CONTAINER_MODE_ENV,
    },
    LayerConfig, MIRRORD_CONFIG_FILE_ENV,
};
use mirrord_progress::{Progress, ProgressTracker, MIRRORD_PROGRESS_ENV};
use service::ServiceInfo;
use steps::*;
use tempfile::NamedTempFile;
use tokio::process::Command;
use tracing::Level;

use super::{create_temp_layer_config, prepare_tls_certs_for_container};
use crate::{
    config::RuntimeArgs,
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::CONTAINER_EXECUTION_KIND,
    error::ContainerError,
    execution::{LINUX_INJECTION_ENV_VAR, MIRRORD_CONNECT_TCP_ENV, MIRRORD_EXECUTION_KIND_ENV},
    util::MIRRORD_CONSOLE_ADDR_ENV,
    ContainerRuntime, ExecParams, MirrordExecution,
};

type ComposeResult<T> = Result<T, ComposeError>;

mod composer;
mod error;
mod service;
mod steps;

#[derive(Debug)]
struct TlsFileGuard {
    cert: NamedTempFile,
    key: NamedTempFile,
}

impl From<(NamedTempFile, NamedTempFile)> for TlsFileGuard {
    fn from(value: (NamedTempFile, NamedTempFile)) -> Self {
        Self {
            cert: value.0,
            key: value.1,
        }
    }
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
    ) -> ComposeResult<ComposeRunner<PrepareConfig>> {
        let RuntimeArgs { runtime, command } = runtime_args;

        let progress = ProgressTracker::from_env("mirrord container");

        if command.has_publish() {
            progress.warning("mirrord container may have problems with \"-p\" when used as part of container run command, please add the publish arguments to \"contanier.cli_extra_args\" in config if you are planning to publish ports");
        }

        progress.warning("mirrord container is currently an unstable feature");

        for (key, value) in exec_params.as_env_vars()? {
            std::env::set_var(key, value);
        }

        std::env::set_var(
            MIRRORD_EXECUTION_KIND_ENV,
            (CONTAINER_EXECUTION_KIND as u32).to_string(),
        );

        let runtime_args = vec![vec!["compose".to_string()], command.into_parts().1].concat();

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: PrepareConfig,
        })
    }
}

impl ComposeRunner<PrepareConfig> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) fn layer_config_and_analytics(
        self,
        watch: drain::Watch,
    ) -> ComposeResult<ComposeRunner<PrepareExternalProxy>> {
        let Self {
            progress,
            runtime,
            runtime_args,
            step: _,
        } = self;

        let (mut config, mut context) = LayerConfig::from_env_with_warnings()?;

        config.verify(&mut context)?;
        for warning in context.get_warnings() {
            progress.warning(warning);
        }

        let (internal_proxy_tls_guards, external_proxy_tls_guards) =
            prepare_tls_certs_for_container(&mut config)?;

        let layer_config_file = create_temp_layer_config(&config)?;
        std::env::set_var(MIRRORD_CONFIG_FILE_ENV, layer_config_file.path());

        // Initialize only error analytics, extproxy will be the full AnalyticsReporter.
        let analytics =
            AnalyticsReporter::only_error(config.telemetry, CONTAINER_EXECUTION_KIND, watch);

        Ok(ComposeRunner {
            runtime,
            runtime_args,
            progress,
            step: PrepareExternalProxy {
                config,
                layer_config_file,
                analytics,
                internal_proxy_tls_guards: internal_proxy_tls_guards.map(From::from),
                external_proxy_tls_guards: external_proxy_tls_guards.map(From::from),
            },
        })
    }
}

impl ComposeRunner<PrepareExternalProxy> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn external_proxy(self) -> ComposeResult<ComposeRunner<PrepareServices>> {
        let Self {
            progress,
            runtime,
            runtime_args,
            step:
                PrepareExternalProxy {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    mut analytics,
                    config,
                    layer_config_file,
                },
        } = self;

        let mut external_proxy_progress = progress.subtask("preparing external proxy");
        let external_proxy =
            MirrordExecution::start_external(&config, &mut external_proxy_progress, &mut analytics)
                .await?;

        external_proxy_progress.success(None);

        let intproxy_address = config
            .internal_proxy
            .connect_tcp
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8888));

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: PrepareServices {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                layer_config_file,
                intproxy_address,
                external_proxy,
                config,
            },
        })
    }
}

impl ComposeRunner<PrepareServices> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) fn services(self) -> ComposeResult<ComposeRunner<PrepareCompose>> {
        let Self {
            progress,
            runtime,
            runtime_args,
            step:
                PrepareServices {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    analytics,
                    config,
                    intproxy_address,
                    layer_config_file,
                    external_proxy,
                },
        } = self;

        let mut user_service_info = ServiceInfo::default();

        if let Ok(console_addr) = std::env::var(MIRRORD_CONSOLE_ADDR_ENV) {
            if console_addr
                .parse()
                .map(|addr: SocketAddr| !addr.ip().is_loopback())
                .unwrap_or_default()
            {
                user_service_info
                    .env_vars
                    .insert(MIRRORD_CONSOLE_ADDR_ENV.into(), console_addr);
            } else {
                // TODO(alex) [mid]: Use eth0 ip.
                tracing::warn!(
                ?console_addr,
                "{MIRRORD_CONSOLE_ADDR_ENV} needs to be a non loopback address when used with containers"
            );
            }
        }

        user_service_info
            .env_vars
            .insert(MIRRORD_PROGRESS_ENV.into(), "off".into());
        user_service_info.env_vars.insert(
            MIRRORD_EXECUTION_KIND_ENV.into(),
            (CONTAINER_EXECUTION_KIND as u32).to_string(),
        );

        user_service_info.env_vars.insert(
            MIRRORD_CONFIG_FILE_ENV.into(),
            "/tmp/mirrord-config.json".into(),
        );

        let mut sidecar_info = user_service_info.clone();

        sidecar_info.volumes.insert(
            layer_config_file.path().to_string_lossy().into(),
            "/tmp/mirrord-config.json".into(),
        );

        let mut load_env_and_mount_pem = |env: &str, path: &Path| {
            let container_path = format!("/tmp/{}.pem", env.to_lowercase());

            user_service_info
                .env_vars
                .insert(env.into(), container_path.clone());
            sidecar_info
                .env_vars
                .insert(env.into(), container_path.clone());

            sidecar_info.volumes.insert(
                path.to_string_lossy().into(),
                format!("{}:ro", container_path),
            );
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

        sidecar_info
            .env_vars
            .insert(MIRRORD_INTPROXY_CONTAINER_MODE_ENV.into(), "true".into());

        let mut connection_info = Vec::new();
        let mut execution_info_env_without_connection_info = Vec::new();
        for (key, value) in external_proxy.environment.iter() {
            if key == MIRRORD_CONNECT_TCP_ENV {
                connection_info.push((key.as_str(), value.as_str()));
                execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
            } else if key == AGENT_CONNECT_INFO_ENV_KEY {
                connection_info.push((key.as_str(), value.as_str()));
            } else {
                execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
            }
        }

        sidecar_info.env_vars.extend(
            connection_info
                .clone()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );

        user_service_info.env_vars.extend(
            execution_info_env_without_connection_info
                .clone()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );

        user_service_info.env_vars.insert(
            MIRRORD_CONNECT_TCP_ENV.to_string(),
            intproxy_address.to_string(),
        );

        user_service_info.env_vars.insert(
            LINUX_INJECTION_ENV_VAR.to_string(),
            config.container.cli_image_lib_path.to_string_lossy().into(),
        );

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: PrepareCompose {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                layer_config_file,
                intproxy_port: intproxy_address.port(),
                sidecar_info,
                user_service_info,
            },
        })
    }
}

const MIRRORD_COMPOSE_SIDECAR_SERVICE: &str = "mirrord-sidecar";

impl ComposeRunner<PrepareCompose> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn make_temp_compose_file(self) -> ComposeResult<ComposeRunner<RunCompose>> {
        use docker_compose_types::{Port, Ports, PublishedPort, Service, Volumes};

        let mut compose = self.read_user_compose_file().await?;

        let Self {
            progress,
            runtime,
            runtime_args,
            step:
                PrepareCompose {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    analytics,
                    layer_config_file,
                    intproxy_port,
                    sidecar_info,
                    user_service_info,
                },
        } = self;

        // `Ports::Short` by default, so build explicitly it as `Ports::Long`.
        let mut sidecar_ports = Ports::Long(Default::default());

        for (service, _) in compose
            .services
            .0
            .iter_mut()
            .filter_map(|(name, service)| service.as_mut().zip(Some(name)))
        {
            let user_service_ports =
                ServiceComposer::modify(service, &user_service_info).reset_conflicts();

            // When a service has no `ports`, it gets built by default as `Ports::Short`,
            // which we are ignoring here (ignore on empty).
            if let (Ports::Long(ports), Ports::Long(current)) =
                (&mut sidecar_ports, user_service_ports)
            {
                ports.extend(current);
            }
        }

        let mirrord_sidecar_service = Service {
            image: Some("ghcr.io/metalbear-co/mirrord-cli:3.133.1".into()),
            command: Some(docker_compose_types::Command::Simple(format!(
                "mirrord intproxy --port {intproxy_port}"
            ))),
            ports: {
                match &mut sidecar_ports {
                    Ports::Long(ports) => {
                        ports.push(Port {
                            target: 5000,
                            host_ip: Some("0.0.0.0".into()),
                            published: Some(PublishedPort::Single(8000)),
                            protocol: Some("tcp".into()),
                            mode: Some("ingress".into()),
                        });
                    }
                    _ => unreachable!("BUG! `sidecar_ports` was constructed as `Ports::Long`!"),
                }

                sidecar_ports
            },
            environment: {
                docker_compose_types::Environment::List(
                    sidecar_info
                        .env_vars
                        .into_iter()
                        // TODO(alex) [mid] [#4]: Remove this.
                        .filter(|(k, _)| !k.contains("LOCALSTACK"))
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect(),
                )
            },
            volumes: {
                sidecar_info
                    .volumes
                    .iter()
                    .map(|(k, v)| Volumes::Simple(format!("{k}:{v}")))
                    .collect()
            },
            ..Default::default()
        };

        compose.services.0.insert(
            MIRRORD_COMPOSE_SIDECAR_SERVICE.into(),
            Some(mirrord_sidecar_service),
        );

        let mut compose_yaml = tempfile::Builder::new()
            .prefix("mirrord-compose-")
            .suffix(".yaml")
            .tempfile()
            .map_err(ContainerError::ConfigWrite)?;

        compose_yaml.write_all(serde_yaml::to_string(&compose)?.as_bytes())?;

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: RunCompose {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                layer_config_file,
                compose_yaml,
            },
        })
    }

    #[tracing::instrument(level = Level::DEBUG, skip(self), ret, err)]
    async fn read_user_compose_file(&self) -> ComposeResult<docker_compose_types::Compose> {
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

        let compose_config = Command::new(self.runtime.to_string())
            .args(vec![
                "compose",
                "--file",
                &compose_file_path.to_string_lossy(),
                "config",
            ])
            .output()
            .await?;

        Ok(serde_yaml::from_slice(&compose_config.stdout)?)
    }
}

impl ComposeRunner<RunCompose> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn debug_stuff(self) -> ComposeResult<ComposeRunner<()>> {
        let Self {
            progress,
            runtime,
            runtime_args,
            step:
                RunCompose {
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards: _,
                    mut analytics,
                    layer_config_file,
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

        if let Some(TlsFileGuard { cert, key }) = internal_proxy_tls_guards {
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
