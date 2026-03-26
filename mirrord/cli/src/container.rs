#[cfg(not(target_os = "windows"))]
use std::os::unix::process::ExitStatusExt;
use std::{net::SocketAddr, ops::Not, path::PathBuf, process::Stdio};

use clap::ValueEnum;
pub use command_display::CommandDisplay;
use command_display::CommandExt;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, ExecutionKind, Reporter};
use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, config::ConfigContext,
    external_proxy::MIRRORD_EXTPROXY_TLS_SERVER_NAME,
};
use mirrord_progress::Progress;
use mirrord_tls_util::SecureChannelSetup;
pub use sidecar::IntproxySidecarError;
use tokio::process::Command;
use tracing::Level;

use crate::{
    CliError, MirrordCi,
    ci::MirrordCiManagedContainer,
    config::{ContainerRuntime, ExecParams, RuntimeArgs},
    container::{command_builder::RuntimeCommandBuilder, sidecar::IntproxySidecar},
    ensure_not_nested,
    error::{CliResult, ContainerError},
    execution::{LINUX_INJECTION_ENV_VAR, MirrordExecution},
    logging::pipe_intproxy_sidecar_logs,
    user_data::UserData,
    util::MIRRORD_CONSOLE_ADDR_ENV,
    wsl::adjust_container_config_for_wsl,
};

mod command_builder;
mod command_display;
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
        panic!(
            "{MIRRORD_CONSOLE_ADDR_ENV} needs to be a non loopback address when used with containers: {console_addr}"
        )
    }
}

/// Resolves the architecture-specific library path based on the platform configuration.
///
/// When a platform is specified (e.g., "linux/amd64"), this function returns the
/// architecture-specific path within the multi-architecture container.
/// If no platform is specified, returns the default path.
fn resolve_library_path(config: &LayerConfig) -> CliResult<String> {
    if let Some(path) = &config.container.cli_image_lib_path {
        return Ok(path.clone());
    }

    if let Some(platform) = &config.container.platform {
        // Extract architecture from platform string (e.g., "linux/amd64" -> "amd64")
        let arch = platform
            .split('/')
            .next_back()
            .ok_or_else(|| ContainerError::UnsupportedPlatform(platform.clone()))?;
        let arch_suffix = match arch {
            "amd64" | "x86_64" => "x86_64",
            "arm64" | "aarch64" => "aarch64",
            _ => Err(ContainerError::UnsupportedPlatform(arch.to_string()))?,
        };

        // Return architecture-specific path
        return Ok(format!(
            "/opt/mirrord/lib/{}/libmirrord_layer.so",
            arch_suffix
        ));
    }

    Ok("/opt/mirrord/lib/libmirrord_layer.so".to_string())
}

/// Resolves the [`LayerConfig`] and creates [`AnalyticsReporter`] whilst reporting any warnings.
///
/// Uses [`ExecutionKind::Container`] to create the [`AnalyticsReporter`].
///
/// Uses the given `progress` to pass warnings from [`LayerConfig`] verification.
async fn create_config_and_analytics<P: Progress>(
    progress: &mut P,
    mut cfg_context: ConfigContext,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<(LayerConfig, AnalyticsReporter)> {
    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    crate::profile::apply_profile_if_configured(&mut config, progress).await?;

    // Initialize only error analytics, extproxy will be the full AnalyticsReporter.
    let analytics = AnalyticsReporter::only_error(
        config.telemetry,
        ExecutionKind::Container,
        watch,
        user_data.machine_id(),
    );

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    Ok((config, analytics))
}

#[derive(Debug)]
struct PreparedProxies {
    runtime_command: RuntimeCommandBuilder,
    execution_info: MirrordExecution,
    tls_setup: Option<SecureChannelSetup>,
    sidecar_pid: Option<u32>,
    sidecar_container: MirrordCiManagedContainer,
}

/// Makes the agent connection, spawns the native `mirrord-extproxy` process,
/// and starts the `mirrord-intproxy` sidecar container.
///
/// # Returns
///
/// 1. Prepared command to run the user container.
/// 2. Handle to the external proxy.
/// 3. Handle to temporary files containing intproxy-extproxy TLS configs.
async fn prepare_proxies<P: Progress>(
    analytics: &mut AnalyticsReporter,
    progress: &P,
    config: &mut LayerConfig,
    runtime: ContainerRuntime,
    mirrord_for_ci: Option<&MirrordCi>,
) -> CliResult<PreparedProxies> {
    let tls_setup = config
        .external_proxy
        .tls_enable
        .then(|| SecureChannelSetup::try_new(MIRRORD_EXTPROXY_TLS_SERVER_NAME, "intproxy"))
        .transpose()
        .map_err(ContainerError::from)?;

    let mut sub_progress = progress.subtask("preparing to launch process");
    let (execution_info, extproxy_addr) = MirrordExecution::start_external(
        config,
        &mut sub_progress,
        analytics,
        tls_setup.as_ref(),
        mirrord_for_ci,
    )
    .await?;
    sub_progress.success(None);

    let extproxy_addr = config
        .container
        .override_host_ip
        .map(|host_ip| SocketAddr::new(host_ip, extproxy_addr.port()))
        .unwrap_or(extproxy_addr);

    let sidecar =
        IntproxySidecar::create(config, runtime, extproxy_addr, tls_setup.as_ref()).await?;

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
    runtime_command.add_env(LINUX_INJECTION_ENV_VAR, &resolve_library_path(config)?);
    runtime_command.add_env(LayerConfig::RESOLVED_CONFIG_ENV, &config.encode()?);

    // Add platform specification if configured
    if let Some(platform) = &config.container.platform {
        runtime_command.add_platform(platform);
    }

    let sidecar_container = MirrordCiManagedContainer {
        runtime,
        container_id: sidecar.container_id().to_string(),
    };

    let (sidecar_intproxy_address, sidecar_intproxy_logs) = sidecar.start().await?;
    let sidecar_pid = sidecar_intproxy_logs.pid();
    let intproxy_logs_pipe =
        pipe_intproxy_sidecar_logs(config, sidecar_intproxy_logs.into_merged_lines()).await?;
    tokio::spawn(intproxy_logs_pipe);

    // Provide internal proxy address to the layer.
    runtime_command.add_env(
        MIRRORD_LAYER_INTPROXY_ADDR,
        &sidecar_intproxy_address.to_string(),
    );

    Ok(PreparedProxies {
        runtime_command,
        execution_info,
        tls_setup,
        sidecar_pid,
        sidecar_container,
    })
}

/// Main entry point for the `mirrord container` command.
///
/// 1. Spawns the agent (cluster), mirrord-extproxy (natively), and mirrord-intproxy (sidecar).
/// 2. Adds additional env, volume, and network to the user container command.
/// 3. Executes the user container command.
#[cfg_attr(windows, allow(unused))]
#[tracing::instrument(level = Level::DEBUG, skip(watch, progress), ret, err(level = Level::DEBUG, Debug))]
pub(crate) async fn container_command<P: Progress>(
    runtime_args: RuntimeArgs,
    exec_params: ExecParams,
    watch: drain::Watch,
    user_data: &UserData,
    progress: &mut P,
    mirrord_for_ci: Option<MirrordCi>,
) -> CliResult<i32> {
    ensure_not_nested()?;

    if runtime_args.command.has_publish() {
        progress.warning(
            "mirrord container may have problems with \"-p\" when used as part of container run command. \
            If you want to publish ports, please add the publish arguments to the
            \"container.cli_extra_args\" list in your mirrord config.",
        );
    }

    progress.warning("mirrord container is currently an unstable feature");

    let cfg_context = ConfigContext::default().override_envs(exec_params.as_env_vars());
    let (mut config, mut analytics) =
        create_config_and_analytics(progress, cfg_context, watch, user_data).await?;

    adjust_container_config_for_wsl(runtime_args.runtime, &mut config);

    let PreparedProxies {
        runtime_command,
        execution_info,
        tls_setup: _,
        sidecar_pid,
        sidecar_container,
    } = prepare_proxies(
        &mut analytics,
        progress,
        &mut config,
        runtime_args.runtime,
        mirrord_for_ci.as_ref(),
    )
    .await?;

    let (binary, binary_args) = runtime_command
        .with_command(runtime_args.command)
        .into_command_args();
    let binary_args = binary_args.collect::<Vec<_>>();

    let extproxy_pid = execution_info.child_id();

    let exit_code = match mirrord_for_ci {
        #[cfg(not(target_os = "windows"))]
        Some(mirrord_ci) => mirrord_ci
            .prepare_container_command(
                progress,
                &binary,
                &binary_args,
                extproxy_pid,
                sidecar_pid,
                Some(sidecar_container),
                &config.ci,
            )
            .await
            .map_err(CliError::from)?,
        #[cfg(target_os = "windows")]
        Some(_) => {
            progress.failure(Some("Command not supported on windows!"));
            return Err(CliError::UnsupportedOnWindows(
                "BUG: somehow `mirrord ci container` was started on windows! \
                Please report this bug to us!"
                    .to_string(),
            ));
        }
        None => {
            progress.success(None);

            let mut runtime_command = Command::new(&binary);
            runtime_command
                .args(&binary_args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());

            let status = runtime_command.status().await.map_err(|error| {
                analytics.set_error(AnalyticsError::BinaryExecuteFailed);

                ContainerError::CommandExec {
                    error,
                    command: runtime_command.display(),
                }
            })?;

            #[cfg(not(target_os = "windows"))]
            if let Some(signal) = status.signal() {
                tracing::warn!("Container command was terminated by signal {signal}");
                -1
            } else {
                status.code().unwrap_or_default()
            }

            #[cfg(target_os = "windows")]
            {
                status.code().unwrap_or_default()
            }
        }
    };

    Ok(exit_code)
}

/// Main entry point for the `mirrord container` command.
///
/// 1. Spawns the agent (cluster), mirrord-extproxy (natively), and mirrord-intproxy (sidecar).
/// 2. Adds additional env, volume, and network to the user container command.
/// 3. Outputs the [`ExtensionRuntimeCommand`](command_builder::ExtensionRuntimeCommand) for the
///    extension.
/// 4. Waits for mirrord-extproxy exit.
#[tracing::instrument(level = Level::DEBUG, skip(watch, progress), ret, err(level = Level::DEBUG, Debug))]
pub async fn container_ext_command<P: Progress>(
    config_file: Option<PathBuf>,
    target: Option<String>,
    watch: drain::Watch,
    user_data: &UserData,
    progress: &mut P,
    mirrord_for_ci: Option<MirrordCi>,
) -> CliResult<()> {
    let cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, config_file)
        .override_env_opt("MIRRORD_IMPERSONATED_TARGET", target);
    let (mut config, mut analytics) =
        create_config_and_analytics(progress, cfg_context, watch, user_data).await?;

    let container_runtime = std::env::var("MIRRORD_CONTAINER_USE_RUNTIME")
        .ok()
        .and_then(|value| ContainerRuntime::from_str(&value, true).ok())
        .unwrap_or(ContainerRuntime::Docker);

    adjust_container_config_for_wsl(container_runtime, &mut config);

    let PreparedProxies {
        runtime_command,
        execution_info,
        tls_setup: _,
        sidecar_pid: _,
        sidecar_container: _,
    } = prepare_proxies(
        &mut analytics,
        progress,
        &mut config,
        container_runtime,
        mirrord_for_ci.as_ref(),
    )
    .await?;

    let output = serde_json::to_string(&runtime_command.into_command_extension_params())?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}
