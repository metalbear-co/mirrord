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

mod error;
mod service;
mod steps;

trait ServiceExt {
    fn modify_environment(&mut self, service_info: &ServiceInfo);
    fn modify_depends_on(&mut self);
}

impl ServiceExt for docker_compose_types::Service {
    fn modify_environment(&mut self, service_info: &ServiceInfo) {
        match &mut self.environment {
            docker_compose_types::Environment::List(items) => {
                items.extend(
                    service_info
                        .env_vars
                        .iter()
                        // TODO(alex) [mid] [#4]: Remove this.
                        .filter(|(k, _)| !k.contains("LOCALSTACK"))
                        .map(|(k, v)| format!("{k}:{v}")),
                );
            }
            docker_compose_types::Environment::KvPair(index_map) => {
                index_map.extend(
                    service_info
                        .env_vars
                        .iter()
                        // TODO(alex) [mid] [#4]: Remove this.
                        .filter(|(k, _)| !k.contains("LOCALSTACK"))
                        .map(|(k, v)| (k.to_owned(), serde_yaml::from_str(&format!("{v}")).ok())),
                );
            }
        }
    }

    fn modify_depends_on(&mut self) {
        use docker_compose_types::DependsCondition;

        match &mut self.depends_on {
            docker_compose_types::DependsOnOptions::Simple(items) => {
                items.push(MIRRORD_COMPOSE_SIDECAR_SERVICE.into())
            }
            docker_compose_types::DependsOnOptions::Conditional(index_map) => {
                index_map.insert(
                    MIRRORD_COMPOSE_SIDECAR_SERVICE.into(),
                    DependsCondition {
                        condition: "service_started".to_owned(),
                    },
                );
            }
        }
    }
}

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
    pub(super) fn layer_config(self) -> ComposeResult<ComposeRunner<PrepareAnalytics>> {
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

        Ok(ComposeRunner {
            runtime,
            runtime_args,
            progress,
            step: PrepareAnalytics {
                config,
                layer_config_file,
                internal_proxy_tls_guards: internal_proxy_tls_guards.map(From::from),
                external_proxy_tls_guards: external_proxy_tls_guards.map(From::from),
            },
        })
    }
}

impl ComposeRunner<PrepareAnalytics> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) fn analytics(
        self,
        watch: drain::Watch,
    ) -> ComposeResult<ComposeRunner<PrepareExternalProxy>> {
        let Self {
            progress,
            runtime,
            runtime_args,
            step:
                PrepareAnalytics {
                    config,
                    internal_proxy_tls_guards,
                    external_proxy_tls_guards,
                    layer_config_file,
                },
        } = self;

        // Initialize only error analytics, extproxy will be the full AnalyticsReporter.
        let analytics =
            AnalyticsReporter::only_error(config.telemetry, CONTAINER_EXECUTION_KIND, watch);

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: PrepareExternalProxy {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                config,
                layer_config_file,
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

        // TODO(alex) [high]: Prepare the sidecar command, the `compose.yaml` with `depends_on`
        // added to the user's services, and then `docker compose run`?
        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: PrepareServices {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                layer_config_file,
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

        // TODO(alex) [high]: User service needs `LD_PRELOAD`, right?
        // If so, then it fails with `missing internal proxy address`, even though the address
        // is in the service's env vars.
        //
        // This is what we need? Somehow get the sidecar intproxy's address?
        //
        // runtime_command.add_env(
        //     MIRRORD_CONNECT_TCP_ENV,
        //     sidecar_intproxy_address.to_string(),
        // );
        //
        //
        // TODO(alex) [high]: This is hardcoded, but there's a config for this!
        user_service_info.env_vars.insert(
            MIRRORD_CONNECT_TCP_ENV.to_string(),
            "127.0.0.1:8888".to_string(),
        );

        user_service_info.env_vars.insert(
            LINUX_INJECTION_ENV_VAR.to_string(),
            config.container.cli_image_lib_path.to_string_lossy().into(),
        );

        // TODO(alex) [high] [#2]: Now that we have the user compose file from the start,
        // `user_service_info` is only really holding env vars, `volumes_from` is something
        // we can insert at tmp compose file creation?
        // user_service_info
        //     .volumes_from
        //     .insert("mirrord-sidecar".into());
        // user_service_info.network_mode.replace("".to_string());
        // user_service_info.depends_on.replace("".to_string());

        Ok(ComposeRunner {
            progress,
            runtime,
            runtime_args,
            step: PrepareCompose {
                internal_proxy_tls_guards,
                external_proxy_tls_guards,
                analytics,
                layer_config_file,
                sidecar_info,
                user_service_info,
            },
        })
    }
}

const MIRRORD_COMPOSE_SIDECAR_SERVICE: &str = "mirrord-sidecar";

// TODO(alex) [high] [#1]: This and `PrepareServices` are kinda intermixed, I want `ServiceInfo` to
// have the user stuff, but we only read the user compose file here, AFTER `ServiceInfo` has been
// created.
//
// I think it should be `read->service_info(user + remove obvious conflicts)->add our stuff`.
impl ComposeRunner<PrepareCompose> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub(super) async fn make_temp_compose_file(self) -> ComposeResult<ComposeRunner<RunCompose>> {
        use docker_compose_types::{Port, Ports, PublishedPort, Service, Volumes};

        let mut compose = self.read_user_compose_file()?;
        tracing::debug!(?compose, "User compose file.");

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
                    sidecar_info,
                    user_service_info,
                },
        } = self;

        let intproxy_port = sidecar_info
            .env_vars
            .get("MIRRORD_CONNECT_TCP")
            .and_then(|address| address.split_terminator(":").last())
            .map(|port| port.parse())
            .transpose()?
            .unwrap_or(8888);

        let mut user_ports = None;

        // TODO(alex) [high] [#5]: Abstract this as a builder-like typestate-y thing, since there's
        // some finnickyness.
        for (service, _) in compose
            .services
            .0
            .iter_mut()
            .filter_map(|(n, s)| s.as_mut().zip(Some(n)))
        {
            service.modify_environment(&user_service_info);
            service.modify_depends_on();

            service
                .volumes_from
                .push(MIRRORD_COMPOSE_SIDECAR_SERVICE.into());
            service.network_mode = Some(format!("service:{MIRRORD_COMPOSE_SIDECAR_SERVICE}"));

            user_ports = Some(service.ports.clone());

            // Do NOT reset ports before assigning it to the outer scopped `user_ports`!
            service.ports = Default::default();
            service.networks = Default::default();
        }

        let mut mirrord_sidecar_service: Service = serde_yaml::from_str(&format!(
            r#"
                image: ghcr.io/metalbear-co/mirrord-cli:3.133.1
                command: mirrord intproxy --port 8888
                ports:
                  - "8000:5000"
            "#
        ))?;

        match (&mut mirrord_sidecar_service.ports, user_ports) {
            (Ports::Short(items), Some(Ports::Short(user_ports))) => {
                items.extend(user_ports);
            }
            (Ports::Short(_), Some(Ports::Long(user_ports))) => {
                let long_sidecar_port = Port {
                    target: 5000,
                    host_ip: Some("0.0.0.0".to_owned()),
                    published: Some(PublishedPort::Single(8000)),
                    protocol: Some("tcp".to_owned()),
                    mode: Some("ingress".to_owned()),
                };

                let mut ports = vec![long_sidecar_port];
                ports.extend(user_ports);

                mirrord_sidecar_service.ports = Ports::Long(ports);
            }
            (Ports::Short(_), None) => (),
            _ => unreachable!("BUG! We set the `ports` using the short syntax!"),
        }

        match &mut mirrord_sidecar_service.environment {
            docker_compose_types::Environment::List(items) => items.extend(
                sidecar_info
                    .env_vars
                    .iter()
                    // TODO(alex) [mid] [#4]: Remove this.
                    .filter(|(k, _)| !k.contains("LOCALSTACK"))
                    .map(|(k, v)| format!("{k}={v}")),
            ),
            _ => unreachable!("BUG! `environment` defaults to using the short syntax!"),
        }

        // TODO(alex) [high] [#5]: I think file ops/volumes are not working properly.
        // `java.lang.Exception: Path /data/events.jsonl.gz does not exist, but these files do:`
        // I'm not sure it's volume related, the volume is mounted relative path with `./data`.
        //
        // Also name resolution doesn't work properly.

        mirrord_sidecar_service.volumes.extend(
            sidecar_info
                .volumes
                .iter()
                .map(|(k, v)| Volumes::Simple(format!("{k}:{v}"))),
        );

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
    fn read_user_compose_file(&self) -> ComposeResult<docker_compose_types::Compose> {
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

        Ok(serde_yaml::from_reader(reader)?)
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
