use std::{
    io::{BufRead, BufReader, Cursor, Write},
    net::SocketAddr,
};

use exec::execvp;
use local_ip_address::local_ip;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::{LayerConfig, MIRRORD_CONFIG_FILE_ENV};
use mirrord_progress::{Progress, ProgressTracker, MIRRORD_PROGRESS_ENV};
use tempfile::NamedTempFile;
use tokio::process::Command;
use tracing::Level;

use crate::{
    config::{ContainerArgs, ContainerCommand},
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::command_builder::RuntimeCommandBuilder,
    error::{ContainerError, Result},
    execution::{MirrordExecution, LINUX_INJECTION_ENV_VAR},
    util::MIRRORD_CONSOLE_ADDR_ENV,
};

mod command_builder;

/// Env variable mirrord-layer uses to connect to intproxy
static MIRRORD_CONNECT_TCP_ENV_VAR: &str = "MIRRORD_CONNECT_TCP";

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
        .map_err(|error| ContainerError::UnableParseCommandStdout(format_command(command), error))?
        .ok_or_else(|| {
            let message = (!result.stderr.is_empty())
                .then(|| String::from_utf8(result.stderr).ok())
                .flatten()
                .unwrap_or_else(|| "stdout and stderr were empty".to_owned());

            ContainerError::UnsuccesfulCommandOutput(format_command(command), message)
        })
}

/// Create a temfile with a json serialized [`LayerConfig`] to be loaded by container and external
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
    #[allow(clippy::indexing_slicing)]
    let sidecar_container_id =
        exec_and_get_first_line(Command::new(&runtime_binary).args(&sidecar_args[1..])).await?;

    // After spawning sidecar with -d flag it prints container_id, now we need the address of
    // intproxy running in sidecar to be used by mirrord-layer in execution container
    let intproxy_address: SocketAddr =
        exec_and_get_first_line(Command::new(runtime_binary).args(["logs", &sidecar_container_id]))
            .await?
            .parse()
            .map_err(ContainerError::UnableParseProxySocketAddr)?;

    Ok((sidecar_container_id, intproxy_address))
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
        let (internal_proxy_cert, internal_proxy_key) =
            create_self_signed_certificate(vec!["intproxy".to_owned()])?;

        config
            .external_proxy
            .client_tls_certificate
            .replace(internal_proxy_cert.path().to_path_buf());
        config
            .external_proxy
            .client_tls_key
            .replace(internal_proxy_key.path().to_path_buf());

        Some((internal_proxy_cert, internal_proxy_key))
    } else {
        None
    };

    let _external_proxy_tls_guards = if config.external_proxy.tls_certificate.is_none()
        || config.external_proxy.tls_key.is_none()
    {
        let external_proxy_subject_alt_names = local_ip()
            .map(|item| item.to_string())
            .into_iter()
            .collect();

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
        if key == MIRRORD_CONNECT_TCP_ENV_VAR || key == AGENT_CONNECT_INFO_ENV_KEY {
            connection_info.push((key.as_str(), value.as_str()));
        } else {
            execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
        }
    }

    sub_progress.success(None);

    let mut runtime_command = RuntimeCommandBuilder::new(args.runtime);

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

    runtime_command.add_env(MIRRORD_CONFIG_FILE_ENV, "/tmp/mirrord-config.json");
    runtime_command.add_volume(composed_config_file.path(), "/tmp/mirrord-config.json");

    for (env, path) in config.external_proxy.as_tls_envs() {
        let container_path = format!("/tmp/{}.pem", env.to_lowercase());

        runtime_command.add_env(env, &container_path);
        runtime_command.add_volume(path, container_path);
    }

    runtime_command.add_envs(execution_info_env_without_connection_info);

    let (sidecar_container_id, sidecar_intproxy_address) =
        create_sidecar_intproxy(&config, &runtime_command, connection_info).await?;

    runtime_command.add_network(format!("container:{sidecar_container_id}"));
    runtime_command.add_volumes_from(sidecar_container_id);

    runtime_command.add_env(LINUX_INJECTION_ENV_VAR, config.container.cli_image_lib_path);
    runtime_command.add_env(
        MIRRORD_CONNECT_TCP_ENV_VAR,
        sidecar_intproxy_address.to_string(),
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
