use std::{
    io::{BufRead, BufReader, Cursor, Write},
    net::SocketAddr,
    path::Path,
    time::Duration,
};

use exec::execvp;
use mirrord_analytics::{
    AnalyticsError, AnalyticsReporter, CollectAnalytics, ExecutionKind, Reporter,
};
use mirrord_config::{
    external_proxy::{MIRRORD_EXTERNAL_TLS_CERTIFICATE_ENV, MIRRORD_EXTERNAL_TLS_KEY_ENV},
    internal_proxy::{
        MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE_ENV, MIRRORD_INTPROXY_CLIENT_TLS_KEY_ENV,
        MIRRORD_INTPROXY_CONTAINER_MODE_ENV,
    },
    LayerConfig, MIRRORD_CONFIG_FILE_ENV,
};
use mirrord_progress::{Progress, ProgressTracker, MIRRORD_PROGRESS_ENV};
use tempfile::NamedTempFile;
use tokio::process::Command;
use tokio_retry::strategy::{jitter, ExponentialBackoff};
use tracing::Level;

use crate::{
    config::{ContainerArgs, ContainerCommand},
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::command_builder::RuntimeCommandBuilder,
    error::{CliError, ContainerError, Result},
    execution::{
        MirrordExecution, LINUX_INJECTION_ENV_VAR, MIRRORD_CONNECT_TCP_ENV,
        MIRRORD_EXECUTION_KIND_ENV,
    },
    util::MIRRORD_CONSOLE_ADDR_ENV,
};

static CONTAINER_EXECUTION_KIND: ExecutionKind = ExecutionKind::Container;

mod command_builder;

/// Format [`Command`] to look like the executated command (currently without env because we don't
/// use it in these scenarios)
fn format_command(command: &Command) -> String {
    let command = command.as_std();

    std::iter::once(command.get_program())
        .chain(command.get_args())
        .filter_map(|arg| arg.to_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Execute a [`Command`] and read first line from stdout
#[tracing::instrument(level = Level::TRACE, ret)]
async fn exec_and_get_first_line(command: &mut Command) -> Result<Option<String>, ContainerError> {
    let result = command
        .output()
        .await
        .map_err(ContainerError::UnableToExecuteCommand)?;

    let reader = BufReader::new(Cursor::new(result.stdout));

    reader
        .lines()
        .next()
        .transpose()
        .map_err(|error| ContainerError::UnableParseCommandStdout(format_command(command), error))?
        .map(Ok)
        .or_else(|| {
            (!result.stderr.is_empty())
                .then(|| String::from_utf8(result.stderr).ok())
                .flatten()
                .map(|message| {
                    Err(ContainerError::UnsuccesfulCommandOutput(
                        format_command(command),
                        message,
                    ))
                })
        })
        .transpose()
}

/// Create a temp file with a json serialized [`LayerConfig`] to be loaded by container and external
/// proxy
#[tracing::instrument(level = Level::TRACE, ret)]
fn create_composed_config(config: &LayerConfig) -> Result<NamedTempFile, ContainerError> {
    let mut composed_config_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .map_err(ContainerError::ConfigWrite)?;
    composed_config_file
        .write_all(&serde_json::to_vec(config).map_err(ContainerError::ConfigSerialization)?)
        .map_err(ContainerError::ConfigWrite)?;

    Ok(composed_config_file)
}

/// Create a tempfile and write to it a self-signed certificate with the provided subject alt names
#[tracing::instrument(level = Level::TRACE, ret)]
fn create_self_signed_certificate(
    subject_alt_names: Vec<String>,
) -> Result<(NamedTempFile, NamedTempFile), ContainerError> {
    let geerated = rcgen::generate_simple_self_signed(subject_alt_names)
        .map_err(ContainerError::SelfSignedCertificate)?;

    let mut certificate =
        tempfile::NamedTempFile::new().map_err(ContainerError::WriteSelfSignedCertificate)?;
    certificate
        .write_all(geerated.cert.pem().as_bytes())
        .map_err(ContainerError::WriteSelfSignedCertificate)?;

    let mut private_key =
        tempfile::NamedTempFile::new().map_err(ContainerError::WriteSelfSignedCertificate)?;
    private_key
        .write_all(geerated.key_pair.serialize_pem().as_bytes())
        .map_err(ContainerError::WriteSelfSignedCertificate)?;

    Ok((certificate, private_key))
}

/// Create a "sidecar" container that is running `mirrord intproxy` that connects to `mirrord
/// extproxy` running on user machine to be used by execution container (via mounting on same
/// network)
#[tracing::instrument(level = Level::TRACE, ret)]
async fn create_sidecar_intproxy(
    config: &LayerConfig,
    base_command: &RuntimeCommandBuilder,
    connection_info: Vec<(&str, &str)>,
) -> Result<(String, SocketAddr)> {
    let mut sidecar_command = base_command.clone();

    sidecar_command.add_env(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "true");
    sidecar_command.add_envs(connection_info);

    let sidecar_container_command =
        ContainerCommand::run(["-d", &config.container.cli_image, "mirrord", "intproxy"]);

    let (runtime_binary, sidecar_args) = sidecar_command
        .with_command(sidecar_container_command)
        .into_command_args();

    let mut sidecar_container_spawn = Command::new(&runtime_binary);
    sidecar_container_spawn.args(sidecar_args);

    let sidecar_container_id = exec_and_get_first_line(&mut sidecar_container_spawn)
        .await?
        .ok_or_else(|| {
            ContainerError::UnsuccesfulCommandOutput(
                format_command(&sidecar_container_spawn),
                "stdout and stderr were empty".to_owned(),
            )
        })?;

    let mut sidecar_watcher_command =
        Command::new(std::env::current_exe().map_err(CliError::CliPathError)?);

    sidecar_watcher_command
        .args(["sidecar-watcher", &runtime_binary, &sidecar_container_id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());

    let _ = sidecar_watcher_command.spawn();

    let mut retry_strategy = ExponentialBackoff::from_millis(100)
        .max_delay(Duration::from_secs(10))
        .map(jitter);

    // After spawning sidecar with -d flag it prints container_id, now we need the address of
    // intproxy running in sidecar to be used by mirrord-layer in execution container
    let intproxy_address: SocketAddr = loop {
        let mut command = Command::new(&runtime_binary);
        command.args(["logs", &sidecar_container_id]);

        match exec_and_get_first_line(&mut command).await? {
            Some(line) => {
                break line
                    .parse()
                    .map_err(ContainerError::UnableParseProxySocketAddr)?;
            }
            None => {
                let backoff_timeout = retry_strategy.next().ok_or_else(|| {
                    ContainerError::UnsuccesfulCommandOutput(
                        format_command(&command),
                        "stdout and stderr were empty and retry max delay exceeded".to_owned(),
                    )
                })?;

                tokio::time::sleep(backoff_timeout).await
            }
        }
    };

    Ok((sidecar_container_id, intproxy_address))
}

/// Main entry point for the `mirrord container` command.
/// This spawns: "agent" - "external proxy" - "intproxy sidecar" - "execution container"
pub(crate) async fn container_command(
    container_args: ContainerArgs,
    watch: drain::Watch,
) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord container");

    progress.warning("mirrord container is currently an unstable feature");

    let (runtime_args, exec_params) = container_args.into_parts();

    for (name, value) in exec_params.as_env_vars()? {
        std::env::set_var(name, value);
    }

    std::env::set_var(
        MIRRORD_EXECUTION_KIND_ENV,
        (CONTAINER_EXECUTION_KIND as u32).to_string(),
    );

    let (mut config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics =
        AnalyticsReporter::only_error(config.telemetry, CONTAINER_EXECUTION_KIND, watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let _internal_proxy_tls_guards = if config.internal_proxy.client_tls_certificate.is_none()
        || config.internal_proxy.client_tls_key.is_none()
    {
        let (internal_proxy_cert, internal_proxy_key) =
            create_self_signed_certificate(vec!["intproxy".to_owned()])?;

        config
            .internal_proxy
            .client_tls_certificate
            .replace(internal_proxy_cert.path().to_path_buf());
        config
            .internal_proxy
            .client_tls_key
            .replace(internal_proxy_key.path().to_path_buf());

        Some((internal_proxy_cert, internal_proxy_key))
    } else {
        None
    };

    let _external_proxy_tls_guards = if config.external_proxy.tls_certificate.is_none()
        || config.external_proxy.tls_key.is_none()
    {
        let external_proxy_subject_alt_names = vec![config.external_proxy.address.clone()];

        let (external_proxy_cert, external_proxy_key) =
            create_self_signed_certificate(external_proxy_subject_alt_names)?;

        config
            .external_proxy
            .tls_certificate
            .replace(external_proxy_cert.path().to_path_buf());
        config
            .external_proxy
            .tls_key
            .replace(external_proxy_key.path().to_path_buf());

        Some((external_proxy_cert, external_proxy_key))
    } else {
        None
    };

    let composed_config_file = create_composed_config(&config)?;
    std::env::set_var(MIRRORD_CONFIG_FILE_ENV, composed_config_file.path());

    let mut sub_progress = progress.subtask("preparing to launch process");

    let execution_info =
        MirrordExecution::start_external(&config, &mut sub_progress, &mut analytics).await?;

    let mut connection_info = Vec::new();
    let mut execution_info_env_without_connection_info = Vec::new();

    for (key, value) in &execution_info.environment {
        if key == MIRRORD_CONNECT_TCP_ENV || key == AGENT_CONNECT_INFO_ENV_KEY {
            connection_info.push((key.as_str(), value.as_str()));
        } else {
            execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
        }
    }

    sub_progress.success(None);

    let mut runtime_command = RuntimeCommandBuilder::new(runtime_args.runtime);

    if let Ok(console_addr) = std::env::var(MIRRORD_CONSOLE_ADDR_ENV) {
        if console_addr
            .parse()
            .map(|addr: SocketAddr| !addr.ip().is_loopback())
            .unwrap_or_default()
        {
            runtime_command.add_env(MIRRORD_CONSOLE_ADDR_ENV, console_addr);
        } else {
            tracing::warn!(
                ?console_addr,
                "{MIRRORD_CONSOLE_ADDR_ENV} needs to be a non loopback address when used with containers"
            );
        }
    }

    runtime_command.add_env(MIRRORD_PROGRESS_ENV, "off");
    runtime_command.add_env(
        MIRRORD_EXECUTION_KIND_ENV,
        (CONTAINER_EXECUTION_KIND as u32).to_string(),
    );

    runtime_command.add_env(MIRRORD_CONFIG_FILE_ENV, "/tmp/mirrord-config.json");
    runtime_command.add_volume(composed_config_file.path(), "/tmp/mirrord-config.json");

    let mut load_env_and_mount_pem = |env: &str, path: &Path| {
        let container_path = format!("/tmp/{}.pem", env.to_lowercase());

        runtime_command.add_env(env, &container_path);
        runtime_command.add_volume(path, container_path);
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

    runtime_command.add_envs(execution_info_env_without_connection_info);

    let (sidecar_container_id, sidecar_intproxy_address) =
        create_sidecar_intproxy(&config, &runtime_command, connection_info).await?;

    runtime_command.add_network(format!("container:{sidecar_container_id}"));
    runtime_command.add_volumes_from(sidecar_container_id);

    runtime_command.add_env(LINUX_INJECTION_ENV_VAR, config.container.cli_image_lib_path);
    runtime_command.add_env(
        MIRRORD_CONNECT_TCP_ENV,
        sidecar_intproxy_address.to_string(),
    );

    let (binary, binary_args) = runtime_command
        .with_command(runtime_args.command)
        .into_execvp_args();

    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    Ok(())
}
