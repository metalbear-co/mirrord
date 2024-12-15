use std::{
    collections::HashMap,
    io::Write,
    net::SocketAddr,
    ops::Not,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use clap::ValueEnum;
use local_ip_address::local_ip;
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
use mirrord_progress::{JsonProgress, Progress, ProgressTracker, MIRRORD_PROGRESS_ENV};
use tempfile::NamedTempFile;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};
use tracing::Level;

use crate::{
    config::{ContainerRuntime, ContainerRuntimeCommand, ExecParams, RuntimeArgs},
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::command_builder::RuntimeCommandBuilder,
    error::{CliError, CliResult, ContainerError},
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
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ContainerError::UnableToExecuteCommand)?;

    tracing::warn!(?child, "spawned watch for child");

    let stdout = child.stdout.take().expect("stdout should be piped");
    let stderr = child.stderr.take().expect("stdout should be piped");

    let result = tokio::time::timeout(Duration::from_secs(30), async {
        BufReader::new(stdout)
            .lines()
            .next_line()
            .await
            .map_err(|error| {
                ContainerError::UnableReadCommandStdout(format_command(command), error)
            })
    })
    .await;

    let _ = child.kill().await;

    match result {
        Err(error) => Err(ContainerError::UnsuccesfulCommandExecute(
            format_command(command),
            error.to_string(),
        )),
        Ok(Err(_)) | Ok(Ok(None)) => {
            let mut stderr_buffer = String::new();
            let stderr_len = BufReader::new(stderr)
                .read_to_string(&mut stderr_buffer)
                .await
                .map_err(|error| {
                    ContainerError::UnableReadCommandStderr(format_command(command), error)
                })?;

            if stderr_len > 0 {
                return Err(ContainerError::UnsuccesfulCommandOutput(
                    format_command(command),
                    stderr_buffer,
                ));
            } else {
                return Err(ContainerError::UnsuccesfulCommandOutput(
                    format_command(command),
                    "stdout and stderr were empty".into(),
                ));
            }
        }
        Ok(result) => result,
    }
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
) -> Result<(String, SocketAddr), ContainerError> {
    let mut sidecar_command = base_command.clone();

    sidecar_command.add_env(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "true");
    sidecar_command.add_envs(connection_info);

    let cleanup = config.container.cli_prevent_cleanup.not().then_some("--rm");

    let sidecar_container_command = ContainerRuntimeCommand::run(
        config
            .container
            .cli_extra_args
            .iter()
            .map(String::as_str)
            .chain(cleanup)
            .chain(["-d", &config.container.cli_image, "mirrord", "intproxy"]),
    );

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

    // For Docker runtime sometimes the sidecar doesn't start so we double check.
    // See [#2927](https://github.com/metalbear-co/mirrord/issues/2927)
    if matches!(base_command.runtime(), ContainerRuntime::Docker) {
        let mut container_inspect_command = Command::new(&runtime_binary);
        container_inspect_command
            .args(["inspect", &sidecar_container_id])
            .stdout(Stdio::piped());

        let container_inspect_output = container_inspect_command.output().await.map_err(|err| {
            ContainerError::UnsuccesfulCommandOutput(
                format_command(&container_inspect_command),
                err.to_string(),
            )
        })?;

        let (container_inspection,) =
            serde_json::from_slice::<(serde_json::Value,)>(&container_inspect_output.stdout)
                .unwrap_or_default();

        let container_status = container_inspection
            .get("State")
            .and_then(|inspect| inspect.get("Status"));

        if container_status
            .map(|status| status == "created")
            .unwrap_or(false)
        {
            let mut container_start_command = Command::new(&runtime_binary);

            container_start_command
                .args(["start", &sidecar_container_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            let _ = container_start_command.status().await.map_err(|err| {
                ContainerError::UnsuccesfulCommandOutput(
                    format_command(&container_start_command),
                    err.to_string(),
                )
            })?;
        }
    }

    // After spawning sidecar with -d flag it prints container_id, now we need the address of
    // intproxy running in sidecar to be used by mirrord-layer in execution container
    let intproxy_address: SocketAddr = {
        let mut attach_command = Command::new(&runtime_binary);
        attach_command.args(["logs", "-f", &sidecar_container_id]);

        match exec_and_get_first_line(&mut attach_command).await? {
            Some(line) => line
                .parse()
                .map_err(ContainerError::UnableParseProxySocketAddr)?,
            None => {
                return Err(ContainerError::UnsuccesfulCommandOutput(
                    format_command(&attach_command),
                    "stdout and stderr were empty".into(),
                ))
            }
        }
    };

    Ok((sidecar_container_id, intproxy_address))
}

type TlsGuard = (NamedTempFile, NamedTempFile);

fn prepare_tls_certs_for_container(
    config: &mut LayerConfig,
) -> CliResult<(Option<TlsGuard>, Option<TlsGuard>)> {
    let internal_proxy_tls_guards = if config.external_proxy.tls_enable
        && (config.internal_proxy.client_tls_certificate.is_none()
            || config.internal_proxy.client_tls_key.is_none())
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

    let external_proxy_tls_guards = if config.external_proxy.tls_enable
        && (config.external_proxy.tls_certificate.is_none()
            || config.external_proxy.tls_key.is_none())
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

    Ok((internal_proxy_tls_guards, external_proxy_tls_guards))
}

/// Main entry point for the `mirrord container` command.
/// This spawns: "agent" - "external proxy" - "intproxy sidecar" - "execution container"
pub(crate) async fn container_command(
    runtime_args: RuntimeArgs,
    exec_params: ExecParams,
    watch: drain::Watch,
) -> CliResult<i32> {
    let mut progress = ProgressTracker::from_env("mirrord container");

    if runtime_args.command.has_publish() {
        progress.warning("mirrord container may have problems with \"-p\" directly container in command, please add to \"contanier.cli_extra_args\" in config if you are planning to publish ports");
    }

    progress.warning("mirrord container is currently an unstable feature");

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

    let (_internal_proxy_tls_guards, _external_proxy_tls_guards) =
        prepare_tls_certs_for_container(&mut config)?;

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
    runtime_command
        .add_volume::<true, _, _>(composed_config_file.path(), "/tmp/mirrord-config.json");

    let mut load_env_and_mount_pem = |env: &str, path: &Path| {
        let container_path = format!("/tmp/{}.pem", env.to_lowercase());

        runtime_command.add_env(env, &container_path);
        runtime_command.add_volume::<true, _, _>(path, container_path);
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

    progress.success(None);

    let (binary, binary_args) = runtime_command
        .with_command(runtime_args.command)
        .into_command_args();

    let runtime_command_result = Command::new(binary)
        .args(binary_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;

    // Keep the files, since this process is going to exit before the new process is actually run
    // and uses them perhaps find a way to clean up? (can't wait for container to boot
    // successfuly since when it loads other processes it might need it too)
    if let Some((cert, key)) = _internal_proxy_tls_guards {
        if let Err(err) = cert.keep() {
            tracing::warn!(?err, "failed to keep internal proxy certificate");
        }

        if let Err(err) = key.keep() {
            tracing::warn!(?err, "failed to keep internal proxy key");
        }
    }

    if let Err(err) = composed_config_file.keep() {
        tracing::warn!(?err, "failed to keep composed config file");
    }

    match runtime_command_result {
        Err(err) => {
            analytics.set_error(AnalyticsError::BinaryExecuteFailed);

            Err(ContainerError::UnableToExecuteCommand(err).into())
        }
        Ok(status) => Ok(status.code().unwrap_or_default()),
    }
}

pub(crate) async fn container_ext_command(
    config_file: Option<PathBuf>,
    target: Option<String>,
    watch: drain::Watch,
) -> CliResult<()> {
    let mut progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());
    let mut env: HashMap<String, String> = HashMap::new();

    if let Some(config_file) = config_file.as_ref() {
        // Set canoncialized path to config file, in case forks/children are in different
        // working directories.
        let full_path = std::fs::canonicalize(config_file)
            .map_err(|e| CliError::CanonicalizeConfigPathFailed(config_file.into(), e))?;
        std::env::set_var(MIRRORD_CONFIG_FILE_ENV, full_path.clone());
        env.insert(
            MIRRORD_CONFIG_FILE_ENV.into(),
            full_path.to_string_lossy().into(),
        );
    }
    if let Some(target) = target.as_ref() {
        std::env::set_var("MIRRORD_IMPERSONATED_TARGET", target.clone());
        env.insert("MIRRORD_IMPERSONATED_TARGET".into(), target.to_string());
    }
    let (mut config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, Default::default(), watch);

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let (_internal_proxy_tls_guards, _external_proxy_tls_guards) =
        prepare_tls_certs_for_container(&mut config)?;

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

    let container_runtime = std::env::var("MIRRORD_CONTAINER_USE_RUNTIME")
        .ok()
        .and_then(|value| ContainerRuntime::from_str(&value, true).ok())
        .unwrap_or(ContainerRuntime::Docker);

    let mut runtime_command = RuntimeCommandBuilder::new(container_runtime);

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
    runtime_command
        .add_volume::<true, _, _>(composed_config_file.path(), "/tmp/mirrord-config.json");

    let mut load_env_and_mount_pem = |env: &str, path: &Path| {
        let container_path = format!("/tmp/{}.pem", env.to_lowercase());

        runtime_command.add_env(env, &container_path);
        runtime_command.add_volume::<true, _, _>(path, container_path);
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

    let output = serde_json::to_string(&runtime_command.into_extension_result())?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}
