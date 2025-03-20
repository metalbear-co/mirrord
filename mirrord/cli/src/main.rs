#![feature(let_chains)]
#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use std::{
    collections::HashMap, env::vars, ffi::CString, net::SocketAddr, os::unix::ffi::OsStrExt,
    time::Duration,
};

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use config::*;
use connection::create_and_connect;
use container::{container_command, container_ext_command};
use diagnose::diagnose_command;
use execution::MirrordExecution;
use extension::extension_exec;
use extract::extract_library;
use mirrord_analytics::{
    AnalyticsError, AnalyticsReporter, CollectAnalytics, ExecutionKind, Reporter,
};
use mirrord_config::{
    config::ConfigContext,
    feature::{
        fs::FsModeConfig,
        network::{
            dns::{DnsConfig, DnsFilterConfig},
            incoming::IncomingMode,
        },
    },
    LayerConfig,
};
use mirrord_intproxy::agent_conn::{AgentConnection, AgentConnectionError};
use mirrord_progress::{messages::EXEC_CONTAINER_BINARY, Progress, ProgressTracker};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use nix::errno::Errno;
use operator::operator_command;
use port_forward::{PortForwardError, PortForwarder, ReversePortForwarder};
use regex::Regex;
use semver::Version;
use tracing::{error, info, warn};
use which::which;

mod config;
mod connection;
mod container;
mod diagnose;
mod error;
mod execution;
mod extension;
mod external_proxy;
mod extract;
mod internal_proxy;
mod list;
mod logging;
mod operator;
mod port_forward;
mod profile;
mod teams;
mod util;
mod verify_config;
mod vpn;

pub(crate) use error::{CliError, CliResult};
use verify_config::verify_config;

async fn exec_process<P>(
    config: LayerConfig,
    config_file_path: Option<&str>,
    args: &ExecArgs,
    progress: &P,
    analytics: &mut AnalyticsReporter,
) -> CliResult<()>
where
    P: Progress + Send + Sync,
{
    let mut sub_progress = progress.subtask("preparing to launch process");

    let execution_info = MirrordExecution::start_internal(
        &config,
        #[cfg(target_os = "macos")]
        Some(&args.binary),
        &mut sub_progress,
        analytics,
    )
    .await?;

    // This is not being yielded, as this is not proper async, something along those lines.
    // We need an `await` somewhere in this function to drive our socket IO that happens
    // in `MirrordExecution::start`. If we don't have this here, then the websocket
    // connection resets, and in the operator you'll get a websocket error.
    tokio::time::sleep(Duration::from_micros(1)).await;

    #[cfg(target_os = "macos")]
    let (_did_sip_patch, binary) = match execution_info.patched_path {
        None => (false, args.binary.clone()),
        Some(ref sip_result) => (true, sip_result.to_owned()),
    };

    #[cfg(not(target_os = "macos"))]
    let binary = args.binary.clone();

    let mut env_vars: HashMap<String, String> = vars().collect();
    env_vars.extend(execution_info.environment.clone());
    env_vars.insert(mirrord_progress::MIRRORD_PROGRESS_ENV.into(), "off".into());
    for key in &execution_info.env_to_unset {
        env_vars.remove(key);
    }

    // Put original executable in argv[0] even if actually running patched version.
    let binary_args = std::iter::once(&args.binary)
        .chain(args.binary_args.iter())
        .map(Clone::clone)
        .collect::<Vec<_>>();

    // since execvpe doesn't exist on macOS, resolve path with which and use execve
    let binary_path = match which(&binary) {
        Ok(pathbuf) => pathbuf,
        Err(error) => return Err(CliError::BinaryWhichError(binary, error.to_string())),
    };
    let path = CString::new(binary_path.as_os_str().as_bytes())?;

    sub_progress.success(Some("ready to launch process"));

    // Print config details for the user
    let mut sub_progress_config = progress.subtask("config summary");
    print_config(
        &sub_progress_config,
        &binary_args,
        &config,
        config_file_path,
        execution_info.uses_operator,
    );
    // Without the success message, the final progress displays the last info message
    // as the subtask title.
    sub_progress_config.success(Some("config summary"));

    let args = binary_args
        .clone()
        .into_iter()
        .map(CString::new)
        .collect::<CliResult<Vec<_>, _>>()?;

    // env vars should be formatted as "varname=value" CStrings
    let env = env_vars
        .into_iter()
        .map(|(k, v)| CString::new(format!("{k}={v}")))
        .collect::<CliResult<Vec<_>, _>>()?;

    // The execve hook is not yet active and does not hijack this call.
    let errno = nix::unistd::execve(&path, args.as_slice(), env.as_slice())
        .expect_err("call to execve cannot succeed");
    error!("Couldn't execute {:?}", errno);
    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if errno == Errno::from_raw(86) {
        // "Bad CPU type in executable"
        if _did_sip_patch {
            return Err(CliError::RosettaMissing(binary));
        }
    }

    Err(CliError::BinaryExecuteFailed(binary, binary_args))
}

/// Prints config summary as multiple info messages, using the given [`Progress`].
fn print_config<P>(
    progress: &P,
    command: &[String],
    config: &LayerConfig,
    config_file_path: Option<&str>,
    operator_used: bool,
) where
    P: Progress + Send + Sync,
{
    progress.info(&format!("Running command: {}", command.join(" ")));

    let target_and_config_path_info = format!(
        "{}, {}",
        match &config.target.path {
            Some(path) => {
                format!("mirrord will target: {}", path)
            }
            None => "mirrord will run without a target".into(),
        },
        match config_file_path {
            Some(path) => {
                format!("the configuration file was loaded from {path}")
            }
            None => "no configuration file was loaded".into(),
        }
    );
    progress.info(&target_and_config_path_info);

    let operator_info = format!(
        "mirrord will run {} the mirrord Operator",
        if operator_used { "with" } else { "without" },
    );
    progress.info(&operator_info);

    let exclude = config.feature.env.exclude.as_ref();
    let include = config.feature.env.include.as_ref();
    let env_info = if let Some(excluded) = exclude {
        if excluded.clone().to_vec().contains(&String::from("*")) {
            "no"
        } else {
            "not all"
        }
    } else if include.is_some() {
        "not all"
    } else {
        "all"
    };
    progress.info(&format!(
        "env: {} remote environment variables will be fetched",
        env_info
    ));

    let fs_info = match config.feature.fs.mode {
        FsModeConfig::Read => "read from the remote",
        FsModeConfig::Write => "read from and write to the remote",
        _ => "read and write locally",
    };
    progress.info(&format!("fs: file operations will default to {}", fs_info));

    let incoming_info = match config.feature.network.incoming.mode {
        IncomingMode::Mirror => "be mirrored",
        IncomingMode::Steal => "be stolen",
        IncomingMode::Off => "ignored",
    };
    progress.info(&format!(
        "incoming: incoming traffic will be {}",
        incoming_info
    ));

    // When the http filter is set, the rules of what ports get stolen are different, so make it
    // clear to users in that case which ports are stolen.
    if config.feature.network.incoming.is_steal()
        && config.feature.network.incoming.http_filter.is_filter_set()
    {
        let filtered_ports_str = config
            .feature
            .network
            .incoming
            .http_filter
            .get_filtered_ports()
            .and_then(|filtered_ports| match filtered_ports.len() {
                0 => None,
                1 => Some(format!(
                    "port {} (filtered)",
                    filtered_ports.first().unwrap()
                )),
                _ => Some(format!("ports {filtered_ports:?} (filtered)")),
            });
        let unfiltered_ports_str =
            config
                .feature
                .network
                .incoming
                .ports
                .as_ref()
                .and_then(|ports| match ports.len() {
                    0 => None,
                    1 => Some(format!(
                        "port {} (unfiltered)",
                        ports.iter().next().unwrap()
                    )),
                    _ => Some(format!(
                        "ports [{}] (unfiltered)",
                        ports
                            .iter()
                            .copied()
                            .map(|n| n.to_string())
                            .collect::<Vec<String>>()
                            .join(", ")
                    )),
                });
        let and = if filtered_ports_str.is_some() && unfiltered_ports_str.is_some() {
            " and "
        } else {
            ""
        };
        let filtered_port_str = filtered_ports_str.unwrap_or_default();
        let unfiltered_ports_str = unfiltered_ports_str.unwrap_or_default();
        progress.info(&format!("incoming: traffic will only be stolen from {filtered_port_str}{and}{unfiltered_ports_str}"));
    }

    let outgoing_info = match (
        config.feature.network.outgoing.tcp,
        config.feature.network.outgoing.udp,
    ) {
        (true, true) => "enabled on TCP and UDP",
        (true, false) => "enabled on TCP",
        (false, true) => "enabled on UDP",
        (false, false) => "disabled on TCP and UDP",
    };
    progress.info(&format!("outgoing: forwarding is {}", outgoing_info));

    let dns_info = match &config.feature.network.dns {
        DnsConfig { enabled: false, .. } => "locally",
        DnsConfig {
            enabled: true,
            filter: None,
        } => "remotely",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Remote(filters)),
        } if filters.is_empty() => "locally",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Local(filters)),
        } if filters.is_empty() => "remotely",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Remote(..)),
        } => "locally with exceptions",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Local(..)),
        } => "remotely with exceptions",
    };
    progress.info(&format!("dns: DNS will be resolved {}", dns_info));
    progress.info(&format!(
        "internal proxy: logs will be written to {}",
        config.internal_proxy.log_destination.display()
    ));
}

async fn exec(args: &ExecArgs, watch: drain::Watch) -> CliResult<()> {
    let progress = ProgressTracker::from_env("mirrord exec");
    if !args.params.disable_version_check {
        prompt_outdated_version(&progress).await;
    }
    info!(
        "Launching {:?} with arguments {:?}",
        args.binary, args.binary_args
    );

    let container_detection =
        Regex::new("docker|podman|nerdctl").expect("Failed building container detection regex!");
    if container_detection.is_match(&args.binary) {
        progress.warning(EXEC_CONTAINER_BINARY);
    }

    if !(args.params.no_tcp_outgoing || args.params.no_udp_outgoing) && args.params.no_remote_dns {
        warn!("TCP/UDP outgoing enabled without remote DNS might cause issues when local machine has IPv6 enabled but remote cluster doesn't")
    }

    let mut cfg_context = ConfigContext::default().override_envs(args.params.as_env_vars());
    let config_file_path = cfg_context.get_env(LayerConfig::FILE_PATH_ENV).ok();
    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    crate::profile::apply_profile_if_configured(&mut config, &progress).await?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, Default::default(), watch);
    (&config).collect_analytics(analytics.get_mut());

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    let execution_result = exec_process(
        config,
        config_file_path.as_deref(),
        args,
        &progress,
        &mut analytics,
    )
    .await;

    if execution_result.is_err() && !analytics.has_error() {
        analytics.set_error(AnalyticsError::Unknown);
    }

    execution_result
}

async fn port_forward(args: &PortForwardArgs, watch: drain::Watch) -> CliResult<()> {
    fn hash_port_mappings(
        args: &PortForwardArgs,
    ) -> CliResult<HashMap<SocketAddr, (RemoteAddr, u16)>, PortForwardError> {
        let port_mappings = &args.port_mapping;
        let mut mappings: HashMap<SocketAddr, (RemoteAddr, u16)> =
            HashMap::with_capacity(port_mappings.len());
        for mapping in port_mappings {
            if mappings
                .insert(mapping.local, mapping.remote.clone())
                .is_some()
            {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::PortMapSetupError(mapping.local));
            }
        }
        Ok(mappings)
    }

    fn hash_rev_port_mappings(
        args: &PortForwardArgs,
    ) -> CliResult<HashMap<RemotePort, LocalPort>, PortForwardError> {
        let port_mappings = &args.reverse_port_mapping;
        let mut mappings: HashMap<RemotePort, LocalPort> =
            HashMap::with_capacity(port_mappings.len());
        for mapping in port_mappings {
            // check destinations are unique
            if mappings.insert(mapping.remote, mapping.local).is_some() {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::ReversePortMapSetupError(mapping.remote));
            }
        }
        Ok(mappings)
    }

    let mut progress = ProgressTracker::from_env("mirrord port-forward");
    progress.warning("Port forwarding is currently an unstable feature and subject to change. See https://github.com/metalbear-co/mirrord/issues/2640 for more info.");

    // validate that mappings have unique local ports and reverse mappings have unique remote ports
    // before we do any more setup, keeping the hashmaps for calling PortForwarder/Reverse
    // it would be nicer to do this with clap but we're limited by the derive interface
    let port_mappings = hash_port_mappings(args)?;
    let rev_port_mappings = hash_rev_port_mappings(args)?;

    if !args.disable_version_check {
        prompt_outdated_version(&progress).await;
    }

    let mut cfg_context = ConfigContext::default()
        .override_envs(args.target.as_env_vars())
        .override_envs(args.agent.as_env_vars())
        .override_env_opt("MIRRORD_TELEMETRY", args.no_telemetry.then_some("false"))
        .override_env_opt(
            "MIRRORD_ACCEPT_INVALID_CERTIFICATES",
            args.accept_invalid_certificates.map(|accept| {
                if accept {
                    warn!("Accepting invalid certificates");
                    "true"
                } else {
                    "false"
                }
            }),
        )
        .override_env_opt("MIRRORD_KUBE_CONTEXT", args.context.as_ref())
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.as_ref());
    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    crate::profile::apply_profile_if_configured(&mut config, &progress).await?;

    let mut analytics = AnalyticsReporter::new(config.telemetry, ExecutionKind::PortForward, watch);
    (&config).collect_analytics(analytics.get_mut());

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    let (connection_info, connection) =
        create_and_connect(&config, &mut progress, &mut analytics).await?;

    // errors from AgentConnection::new get mapped to CliError manually to prevent unreadably long
    // error print-outs
    let agent_conn = AgentConnection::new(&config, connection_info, &mut analytics)
        .await
        .map_err(|agent_con_error| match agent_con_error {
            AgentConnectionError::Io(error) => CliError::PortForwardingSetupError(error.into()),
            AgentConnectionError::Operator(operator_api_error) => operator_api_error.into(),
            AgentConnectionError::Kube(kube_api_error) => CliError::friendlier_error_or_else(
                kube_api_error,
                CliError::PortForwardingSetupError,
            ),
            AgentConnectionError::Tls(connection_tls_error) => connection_tls_error.into(),
        })?;
    let connection_2 = connection::AgentConnection {
        sender: agent_conn.agent_tx,
        receiver: agent_conn.agent_rx,
    };

    let _ = tokio::try_join!(
        async {
            if !args.port_mapping.is_empty() {
                let mut port_forward = PortForwarder::new(connection, port_mappings).await?;
                port_forward.run().await.map_err(|error| error.into())
            } else {
                Ok::<(), CliError>(())
            }
        },
        async {
            if !args.reverse_port_mapping.is_empty() {
                let mut port_forward = ReversePortForwarder::new(
                    connection_2,
                    rev_port_mappings,
                    config.feature.network.incoming,
                    Duration::from_millis(config.experimental.idle_local_http_connection_timeout),
                )
                .await?;
                port_forward.run().await.map_err(|error| error.into())
            } else {
                Ok::<(), CliError>(())
            }
        }
    )?;

    Ok(())
}

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> miette::Result<()> {
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install crypto provider");

    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::RuntimeError)?;

    let (signal, watch) = drain::channel();

    let res: CliResult<(), CliError> = rt.block_on(async move {
        logging::init_tracing_registry(&cli.commands, watch.clone()).await?;

        match cli.commands {
            Commands::Exec(args) => exec(&args, watch).await?,
            Commands::Extract { path } => {
                extract_library(
                    Some(path),
                    &ProgressTracker::from_env("mirrord extract library..."),
                    false,
                )?;
            }
            Commands::ListTargets(args) => {
                let rich_output = std::env::var(ListTargetArgs::RICH_OUTPUT_ENV)
                    .ok()
                    .and_then(|value| value.parse::<bool>().ok())
                    .unwrap_or_default();

                list::print_targets(*args, rich_output).await?
            }
            Commands::Operator(args) => operator_command(*args).await?,
            Commands::ExtensionExec(args) => {
                extension_exec(*args, watch).await?;
            }
            Commands::InternalProxy { port } => {
                let config = mirrord_config::util::read_resolved_config()?;
                logging::init_intproxy_tracing_registry(&config)?;
                internal_proxy::proxy(config, port, watch).await?
            }
            Commands::VerifyConfig(args) => verify_config(args).await?,
            Commands::Completions(args) => {
                let mut cmd: clap::Command = Cli::command();
                generate(args.shell, &mut cmd, "mirrord", &mut std::io::stdout());
            }
            Commands::Teams => teams::navigate_to_intro().await,
            Commands::Diagnose(args) => diagnose_command(*args).await?,
            Commands::Container(args) => {
                let (runtime_args, exec_params) = args.into_parts();

                let exit_code = container_command(runtime_args, exec_params, watch).await?;

                if exit_code != 0 {
                    std::process::exit(exit_code);
                }
            }
            Commands::ExtensionContainer(args) => {
                container_ext_command(args.config_file, args.target, watch).await?
            }
            Commands::ExternalProxy { port } => {
                let config = mirrord_config::util::read_resolved_config()?;
                logging::init_extproxy_tracing_registry(&config)?;
                external_proxy::proxy(config, port, watch).await?
            }
            Commands::PortForward(args) => port_forward(&args, watch).await?,
            Commands::Vpn(args) => vpn::vpn_command(*args).await?,
        };

        Ok(())
    });

    rt.block_on(async move {
        tokio::time::timeout(Duration::from_secs(10), signal.drain())
            .await
            .is_err()
            .then(|| {
                warn!("Failed to drain in a timely manner, ongoing tasks dropped.");
            });
    });

    res.map_err(Into::into)
}

async fn prompt_outdated_version(progress: &ProgressTracker) {
    let mut progress = progress.subtask("version check");
    let check_version: bool = std::env::var("MIRRORD_CHECK_VERSION")
        .map(|s| s.parse().unwrap_or(true))
        .unwrap_or(true);

    if check_version {
        if let Ok(client) = reqwest::Client::builder().build() {
            if let Ok(result) = client
                .get(format!(
                    "https://version.mirrord.dev/get-latest-version?source=2&currentVersion={}&platform={}",
                    CURRENT_VERSION,
                    std::env::consts::OS
                ))
                .timeout(Duration::from_secs(1))
                .send().await
            {
                if let Ok(latest_version) = Version::parse(&result.text().await.unwrap()) {
                    if latest_version > Version::parse(CURRENT_VERSION).unwrap() {
                        let is_homebrew = which("mirrord").ok().map(|mirrord_path| mirrord_path.to_string_lossy().contains("homebrew")).unwrap_or_default();
                        let command = if is_homebrew { "brew upgrade metalbear-co/mirrord/mirrord" } else { "curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash" };
                        progress.print(&format!("New mirrord version available: {}. To update, run: `{:?}`.", latest_version, command));
                        progress.print("To disable version checks, set env variable MIRRORD_CHECK_VERSION to 'false'.");
                        progress.success(Some(&format!("update to {latest_version} available")));
                    } else {
                        progress.success(Some(&format!("running on latest ({CURRENT_VERSION})!")));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use rstest::rstest;

    use crate::{Cli, Commands};

    /// Verifies that
    /// [`ExecParams::accept_invalid_certificates`](crate::config::ExecParams::accept_invalid_certificates)
    /// and [`PortForwardArgs::accept_invalid_certificates`](crate::config::PortForwardArgs::accept_invalid_certificates)
    /// correctly parse from command line arguments.
    #[rstest]
    #[case(&["mirrord", "exec", "-c", "--", "echo", "hello"], Some(true))]
    #[case(&["mirrord", "exec", "-c=true", "--", "echo", "hello"], Some(true))]
    #[case(&["mirrord", "exec", "-c=false", "--", "echo", "hello"], Some(false))]
    #[case(&["mirrord", "exec", "--", "echo", "hello"], None)]
    #[case(&["mirrord", "port-forward", "-c", "-L", "8080:py-serv:80"], Some(true))]
    #[case(&["mirrord", "port-forward", "-c=true", "-L", "8080:py-serv:80"], Some(true))]
    #[case(&["mirrord", "port-forward", "-c=false", "-L", "8080:py-serv:80"], Some(false))]
    #[case(&["mirrord", "port-forward", "-L", "8080:py-serv:80"], None)]
    fn parse_accept_invalid_certificates(
        #[case] args: &[&str],
        #[case] expected_value: Option<bool>,
    ) {
        match Cli::parse_from(args).commands {
            Commands::Exec(params) if *args.get(1).unwrap() == "exec" => {
                assert_eq!(params.params.accept_invalid_certificates, expected_value)
            }
            Commands::PortForward(params) if *args.get(1).unwrap() == "port-forward" => {
                assert_eq!(params.accept_invalid_certificates, expected_value)
            }
            other => panic!("unexpected args parsed: {other:?}"),
        }
    }
}
