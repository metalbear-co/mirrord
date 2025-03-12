use std::{net::SocketAddr, ops::Not, path::PathBuf, process::Stdio};

use clap::ValueEnum;
pub use command_display::CommandDisplay;
use command_display::CommandExt;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, ExecutionKind, Reporter};
use mirrord_config::{
    external_proxy::MIRRORD_EXTPROXY_TLS_SERVER_NAME, LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR,
};
use mirrord_progress::{JsonProgress, Progress, ProgressTracker};
use mirrord_tls_util::SecureChannelSetup;
pub use resolved_config::ResolvedConfigError;
use resolved_config::ResolvedConfigFile;
pub use sidecar::IntproxySidecarError;
use tokio::process::Command;

use crate::{
    config::{ContainerRuntime, ExecParams, RuntimeArgs},
    container::{command_builder::RuntimeCommandBuilder, sidecar::IntproxySidecar},
    error::{CliResult, ContainerError},
    execution::{MirrordExecution, LINUX_INJECTION_ENV_VAR},
    logging::pipe_intproxy_sidecar_logs,
    util::MIRRORD_CONSOLE_ADDR_ENV,
};

mod command_builder;
mod command_display;
mod resolved_config;
mod sidecar;

/// Retrieves the value of [`MIRRORD_CONSOLE_ADDR_ENV`].
///
/// If the variable is not present, returns [`None`].
///
/// # Panic
///
/// This function panics when the variable value is not a non loopback [`SocketAddr`].
///
/// We require that the address is not a loopback,
/// because it has to be accessible from the spawned docker containers.
///
/// It's ok to panic, because mirrord-console is our debugging tool.
fn get_mirrord_console_addr() -> Option<String> {
    let console_addr = std::env::var(MIRRORD_CONSOLE_ADDR_ENV).ok()?;

    let is_non_loopback = console_addr
        .parse::<SocketAddr>()
        .ok()
        .is_some_and(|addr| addr.ip().is_loopback().not());
    if is_non_loopback {
        Some(console_addr)
    } else {
        panic!("{MIRRORD_CONSOLE_ADDR_ENV} needs to be a non loopback address when used with containers: {console_addr}")
    }
}

/// Loads [`LayerConfig`] from env and creates [`AnalyticsReporter`] whilst reporting any warnings.
///
/// Uses [`ExecutionKind::Container`] to create the [`AnalyticsReporter`].
///
/// Uses the given `progress` to pass warnings from [`LayerConfig`] verification.
fn create_config_and_analytics<P: Progress>(
    progress: &mut P,
    watch: drain::Watch,
) -> CliResult<(LayerConfig, AnalyticsReporter)> {
    let (config, mut context) = LayerConfig::resolve()?;

    // Initialize only error analytics, extproxy will be the full AnalyticsReporter.
    let analytics =
        AnalyticsReporter::only_error(config.telemetry, ExecutionKind::Container, watch);

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    Ok((config, analytics))
}

/// Makes the agent connection, spawns the native `mirrord-extproxy` process,
/// and starts the `mirrord-intproxy` sidecar container.
///
/// # Returns
///
/// 1. Prepared command to run the user container.
/// 2. Handle to the external proxy.
async fn prepare_proxies<P: Progress + Send + Sync>(
    analytics: &mut AnalyticsReporter,
    progress: &P,
    config: &LayerConfig,
    runtime: ContainerRuntime,
) -> CliResult<(RuntimeCommandBuilder, MirrordExecution)> {
    let tls_setup = config
        .external_proxy
        .tls_enable
        .then(|| SecureChannelSetup::try_new(MIRRORD_EXTPROXY_TLS_SERVER_NAME, "intproxy"))
        .transpose()
        .map_err(ContainerError::from)?;

    let config_file = ResolvedConfigFile::try_new(config).map_err(ContainerError::from)?;

    let mut sub_progress = progress.subtask("preparing to launch process");
    let (execution_info, extproxy_addr) =
        MirrordExecution::start_external(config, &mut sub_progress, analytics, tls_setup.as_ref())
            .await?;
    sub_progress.success(None);

    let sidecar =
        IntproxySidecar::try_new(config, runtime, extproxy_addr, tls_setup.as_ref()).await?;

    let mut runtime_command = RuntimeCommandBuilder::new(runtime);
    // Provide remote environment to the user application.
    runtime_command.add_envs(
        execution_info
            .environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    // Allow the layer to connect with the internal proxy sidecar.
    runtime_command.add_network(format!("container:{}", sidecar.container_id()));
    // Add the layer file to the user application container.
    runtime_command.add_volumes_from(sidecar.container_id());
    // Inject the layer into the user application.
    runtime_command.add_env(
        LINUX_INJECTION_ENV_VAR,
        &config.container.cli_image_lib_path,
    );
    runtime_command.add_volume(
        config_file.path_str().map_err(ContainerError::from)?,
        "/tmp/mirrord-config",
        true,
    );
    runtime_command.add_env(LayerConfig::FILE_PATH_ENV, "/tmp/mirrord-config");

    let (sidecar_intproxy_address, sidecar_intproxy_logs) = sidecar.start().await?;
    let intproxy_logs_pipe =
        pipe_intproxy_sidecar_logs(&config, sidecar_intproxy_logs.into_merged_lines()).await?;
    tokio::spawn(intproxy_logs_pipe);

    // Provide internal proxy address to the layer.
    runtime_command.add_env(
        MIRRORD_LAYER_INTPROXY_ADDR,
        &sidecar_intproxy_address.to_string(),
    );

    if let Some(setup) = tls_setup {
        setup
            .persist()
            .map_err(ContainerError::ProxyTlsSetupError)?;
    }

    Ok((runtime_command, execution_info))
}

/// Main entry point for the `mirrord container` command.
///
/// This spawns: "agent" - "external proxy" - "intproxy sidecar" - "execution container"
pub async fn container_command(
    runtime_args: RuntimeArgs,
    exec_params: ExecParams,
    watch: drain::Watch,
) -> CliResult<i32> {
    let mut progress = ProgressTracker::from_env("mirrord container");

    if runtime_args.command.has_publish() {
        progress.warning("mirrord container may have problems with \"-p\" when used as part of container run command, please add the publish arguments to \"contanier.cli_extra_args\" in config if you are planning to publish ports");
    }

    progress.warning("mirrord container is currently an unstable feature");

    for (name, value) in exec_params.as_env_vars()? {
        std::env::set_var(name, value);
    }
    // LayerConfig must be created after setting relevant env vars
    let (config, mut analytics) = create_config_and_analytics(&mut progress, watch)?;

    let (runtime_command, _execution_info) =
        prepare_proxies(&mut analytics, &progress, &config, runtime_args.runtime).await?;

    progress.success(None);

    let (binary, binary_args) = runtime_command
        .with_command(runtime_args.command)
        .into_command_args();

    let mut runtime_command = Command::new(binary);
    runtime_command
        .args(binary_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = runtime_command.status().await.map_err(|error| {
        analytics.set_error(AnalyticsError::BinaryExecuteFailed);

        ContainerError::CommandError {
            error,
            command: runtime_command.display(),
        }
    })?;

    Ok(status.code().unwrap_or_default())
}

/// Main entry point for the `mirrord container-ext` command.
///
/// Spawns the intproxy sidecar and the extproxy,
/// returns [`ExtensionRuntimeCommand`](command_builder::ExtensionRuntimeCommand) for the extension.
pub async fn container_ext_command(
    config_file: Option<PathBuf>,
    target: Option<String>,
    watch: drain::Watch,
) -> CliResult<()> {
    let mut progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());

    if let Some(config_file) = config_file.as_ref() {
        std::env::set_var(LayerConfig::FILE_PATH_ENV, config_file);
    }
    if let Some(target) = target.as_ref() {
        std::env::set_var("MIRRORD_IMPERSONATED_TARGET", target.clone());
    }
    // LayerConfig must be created after setting relevant env vars
    let (config, mut analytics) = create_config_and_analytics(&mut progress, watch)?;

    let container_runtime = std::env::var("MIRRORD_CONTAINER_USE_RUNTIME")
        .ok()
        .and_then(|value| ContainerRuntime::from_str(&value, true).ok())
        .unwrap_or(ContainerRuntime::Docker);

    let (runtime_command, execution_info) =
        prepare_proxies(&mut analytics, &mut progress, &config, container_runtime).await?;

    let output = serde_json::to_string(&runtime_command.into_command_extension_params())?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}
