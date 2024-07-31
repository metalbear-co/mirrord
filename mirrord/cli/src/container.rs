use std::{
    io::{BufRead, BufReader, Cursor, Write},
    net::SocketAddr,
};

use exec::execvp;
use local_ip_address::local_ip;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};
use tokio::process::Command;
use tracing::Level;

use crate::{
    config::{ContainerArgs, ContainerCommand},
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::command_builder::RuntimeCommandBuilder,
    error::{ContainerError, Result},
    execution::{MirrordExecution, LINUX_INJECTION_ENV_VAR},
};

mod command_builder;

static MIRRORD_CONNECT_TCP_ENV_VAR: &str = "MIRRORD_CONNECT_TCP";

/// Execute a [`Command`] and read first line from stdout
#[tracing::instrument(level = Level::TRACE, ret)]
async fn exec_and_get_first_line(command: &mut Command) -> Result<String, ContainerError> {
    let result = command
        .output()
        .await
        .map_err(ContainerError::UnableToExecuteCommand)?;

    let reader = BufReader::new(Cursor::new(result.stdout));

    reader
        .lines()
        .next()
        .transpose()
        .map_err(ContainerError::UnableParseCommandStdout)?
        .ok_or_else(|| {
            ContainerError::UnableParseCommandStdout(std::io::Error::other("stdout was empty"))
        })
}

/// Create a "sidecar" container that is running `mirrord intproy` that connects to `mirrord
/// extproxy` running on user machine to be used by execution container (via mounting on same
/// network)
async fn create_sidecar_intproxy(
    config: &LayerConfig,
    base_command: &RuntimeCommandBuilder,
    connection_info: Vec<(&str, &str)>,
) -> Result<(String, SocketAddr), ContainerError> {
    let mut sidecar_command = base_command.clone();

    sidecar_command.add_env("MIRRORD_INTPROXY_DETACH_IO", "false");
    sidecar_command.add_envs(connection_info);

    let sidecar_container_command = ContainerCommand::run([
        "--rm",
        "-d",
        &config.container.cli_image,
        "mirrord",
        "intproxy",
    ]);

    let (runtime_binary, sidecar_args) = sidecar_command
        .with_command(sidecar_container_command)
        .into_execvp_args();

    // We skip first index of sidecar_args because `RuntimeCommandBuilder::into_execvp_args` adds
    // the binary as first arg for `execvp`
    let sidecar_container_id =
        exec_and_get_first_line(Command::new(&runtime_binary).args(&sidecar_args[1..])).await?;

    let intproxt_socket: SocketAddr =
        exec_and_get_first_line(Command::new(runtime_binary).args(["logs", &sidecar_container_id]))
            .await?
            .parse()
            .map_err(ContainerError::UnableParseProxySocketAddr)?;

    Ok((sidecar_container_id, intproxt_socket))
}

/// Main entry point for the `mirrord container` command.
/// This spawns: "agent" - "external proxy" - "intproxy sidecar" - "execution container"
pub(crate) async fn container_command(args: ContainerArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord container");

    for (name, value) in args.params.as_env_vars()? {
        std::env::set_var(name, value);
    }

    let (mut config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let _internal_proxy_tls_guards = if config.external_proxy.client_tls_certificate.is_none()
        || config.external_proxy.client_tls_key.is_none()
    {
        let internal_proxy_tls = rcgen::generate_simple_self_signed(vec!["intproxy".to_owned()])
            .map_err(ContainerError::SelfSignedCertificate)?;

        let mut internal_proxy_cert =
            tempfile::NamedTempFile::new().map_err(ContainerError::WriteSelfSignedCertificate)?;
        internal_proxy_cert
            .write_all(internal_proxy_tls.cert.pem().as_bytes())
            .map_err(ContainerError::WriteSelfSignedCertificate)?;

        config.external_proxy.client_tls_certificate =
            Some(internal_proxy_cert.path().to_path_buf());

        let mut internal_proxy_key =
            tempfile::NamedTempFile::new().map_err(ContainerError::WriteSelfSignedCertificate)?;
        internal_proxy_key
            .write_all(internal_proxy_tls.key_pair.serialize_pem().as_bytes())
            .map_err(ContainerError::WriteSelfSignedCertificate)?;

        config.external_proxy.client_tls_key = Some(internal_proxy_key.path().to_path_buf());

        Some((internal_proxy_cert, internal_proxy_key))
    } else {
        None
    };

    let _external_proxy_tls_guards = if config.external_proxy.tls_certificate.is_none()
        || config.external_proxy.tls_key.is_none()
    {
        let external_proxy_subject_alt_names: Vec<_> = local_ip()
            .into_iter()
            .map(|item| item.to_string())
            .collect();

        let external_proxy_tls =
            rcgen::generate_simple_self_signed(external_proxy_subject_alt_names)
                .map_err(ContainerError::SelfSignedCertificate)?;

        let mut external_proxy_cert =
            tempfile::NamedTempFile::new().map_err(ContainerError::WriteSelfSignedCertificate)?;
        external_proxy_cert
            .write_all(external_proxy_tls.cert.pem().as_bytes())
            .map_err(ContainerError::WriteSelfSignedCertificate)?;

        config.external_proxy.tls_certificate = Some(external_proxy_cert.path().to_path_buf());

        let mut external_proxy_key =
            tempfile::NamedTempFile::new().map_err(ContainerError::WriteSelfSignedCertificate)?;
        external_proxy_key
            .write_all(external_proxy_tls.key_pair.serialize_pem().as_bytes())
            .map_err(ContainerError::WriteSelfSignedCertificate)?;

        config.external_proxy.tls_key = Some(external_proxy_key.path().to_path_buf());

        Some((external_proxy_cert, external_proxy_key))
    } else {
        None
    };

    let mut composed_config_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .map_err(ContainerError::ConfigWrite)?;
    composed_config_file
        .write_all(&serde_json::to_vec(&config).map_err(ContainerError::ConfigSerialization)?)
        .map_err(ContainerError::ConfigWrite)?;

    std::env::set_var("MIRRORD_CONFIG_FILE", composed_config_file.path());

    let mut sub_progress = progress.subtask("preparing to launch process");

    let execution_info =
        MirrordExecution::start_external(&config, &mut sub_progress, &mut analytics).await?;

    let mut connection_info = Vec::new();
    let mut execution_info_env_without_connection_info = Vec::new();

    for (key, value) in &execution_info.environment {
        if key == MIRRORD_CONNECT_TCP_ENV_VAR || key == AGENT_CONNECT_INFO_ENV_KEY {
            connection_info.push((key.as_str(), value.as_str()));
        } else {
            execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
        }
    }

    sub_progress.success(None);

    let mut runtime_command = RuntimeCommandBuilder::new(args.runtime);

    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        if console_addr
            .parse()
            .map(|addr: SocketAddr| !addr.ip().is_loopback())
            .unwrap_or_default()
        {
            runtime_command.add_env("MIRRORD_CONSOLE_ADDR", console_addr);
        } else {
            tracing::warn!(
                ?console_addr,
                "MIRRORD_CONSOLE_ADDR needs to be a non loopback address when used with containers"
            );
        }
    }

    runtime_command.add_env("MIRRORD_PROGRESS_MODE", "off");

    runtime_command.add_env("MIRRORD_CONFIG_FILE", "/tmp/mirrord-config.json");
    runtime_command.add_volume(composed_config_file.path(), "/tmp/mirrord-config.json");

    for (env, path) in config.external_proxy.as_tls_envs() {
        let container_path = format!("/tmp/{}.pem", env.to_lowercase());

        runtime_command.add_env(env, &container_path);
        runtime_command.add_volume(path, container_path);
    }

    runtime_command.add_envs(execution_info_env_without_connection_info);

    let (sidecar_container_id, sidecar_intproxt_socket) =
        create_sidecar_intproxy(&config, &runtime_command, connection_info).await?;

    runtime_command.add_network(format!("container:{sidecar_container_id}"));
    runtime_command.add_volumes_from(sidecar_container_id);

    runtime_command.add_env(LINUX_INJECTION_ENV_VAR, config.container.cli_image_lib_path);
    runtime_command.add_env(
        MIRRORD_CONNECT_TCP_ENV_VAR,
        sidecar_intproxt_socket.to_string(),
    );

    let (binary, binary_args) = runtime_command
        .with_command(args.command)
        .into_execvp_args();

    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    Ok(())
}
