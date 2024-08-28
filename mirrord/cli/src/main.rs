#![feature(let_chains)]
#![feature(lazy_cell)]
#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]

use std::{sync::LazyLock, time::Duration};

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use config::*;
use connection::create_and_connect;
use container::container_command;
use diagnose::diagnose_command;
use exec::execvp;
use execution::MirrordExecution;
use extension::extension_exec;
use extract::extract_library;
use kube::Client;
use miette::JSONReportHandler;
use mirrord_analytics::{
    AnalyticsError, AnalyticsReporter, CollectAnalytics, ExecutionKind, NullReporter, Reporter,
};
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    feature::{
        fs::FsModeConfig,
        network::{
            dns::{DnsConfig, DnsFilterConfig},
            incoming::IncomingMode,
        },
    },
    LayerConfig, LayerFileConfig, MIRRORD_CONFIG_FILE_ENV,
};
use mirrord_kube::api::kubernetes::{create_kube_config, seeker::KubeResourceSeeker};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::{Progress, ProgressTracker};
use operator::operator_command;
use port_forward::PortForwarder;
use semver::{Version, VersionReq};
use serde_json::json;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};
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
mod operator;
pub mod port_forward;
mod teams;
mod util;
mod verify_config;
mod vpn;

pub(crate) use error::{CliError, Result};
use verify_config::verify_config;

use crate::util::remove_proxy_env;

/// Controls whether we support listing all targets or just the open source ones.
static ALL_TARGETS_SUPPORTED_OPERATOR_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=3.84.0".parse().expect("verion should be valid"));

async fn exec_process<P>(
    config: LayerConfig,
    args: &ExecArgs,
    progress: &P,
    analytics: &mut AnalyticsReporter,
) -> Result<()>
where
    P: Progress + Send + Sync,
{
    let mut sub_progress = progress.subtask("preparing to launch process");

    #[cfg(target_os = "macos")]
    let execution_info =
        MirrordExecution::start(&config, Some(&args.binary), &mut sub_progress, analytics).await?;
    #[cfg(not(target_os = "macos"))]
    let execution_info = MirrordExecution::start(&config, &mut sub_progress, analytics).await?;

    #[cfg(target_os = "macos")]
    let (_did_sip_patch, binary) = match execution_info.patched_path {
        None => (false, args.binary.clone()),
        Some(ref sip_result) => (true, sip_result.to_owned()),
    };

    #[cfg(not(target_os = "macos"))]
    let binary = args.binary.clone();

    // Stop confusion with layer
    std::env::set_var(mirrord_progress::MIRRORD_PROGRESS_ENV, "off");

    // Set environment variables from agent + layer settings.
    for (key, value) in &execution_info.environment {
        std::env::set_var(key, value);
    }

    for key in &execution_info.env_to_unset {
        std::env::remove_var(key);
    }

    let mut binary_args = args.binary_args.clone();
    // Put original executable in argv[0] even if actually running patched version.
    binary_args.insert(0, args.binary.clone());

    sub_progress.success(Some("ready to launch process"));

    // Print config details for the user
    let mut sub_progress_config = progress.subtask("config summary");
    print_config(
        &sub_progress_config,
        Some(&binary),
        Some(&args.binary_args),
        &config,
        false,
    );
    sub_progress_config.success(Some("config summary"));

    // The execve hook is not yet active and does not hijack this call.
    let err = execvp(binary.clone(), binary_args.clone());
    error!("Couldn't execute {:?}", err);
    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if let exec::Error::Errno(errno) = err {
        if Into::<i32>::into(errno) == 86 {
            // "Bad CPU type in executable"
            if _did_sip_patch {
                return Err(CliError::RosettaMissing(binary));
            }
        }
    }

    Err(CliError::BinaryExecuteFailed(binary, binary_args))
}

fn print_config<P>(
    progress_subtask: &P,
    binary: Option<&String>,
    binary_args: Option<&Vec<String>>,
    config: &LayerConfig,
    single_msg: bool,
) where
    P: Progress + Send + Sync,
{
    let mut messages = vec![];
    if let Some(b) = binary
        && let Some(a) = binary_args
    {
        messages.push(format!("Running binary \"{}\" with arguments: {:?}.", b, a));
    }

    let target_info = if let Some(target) = &config.target.path {
        &format!("mirrord will target: {}", target)[..]
    } else {
        "mirrord will run without a target"
    };
    let config_info = if let Ok(path) = std::env::var(MIRRORD_CONFIG_FILE_ENV) {
        &format!("a configuration file was loaded from: {} ", path)[..]
    } else {
        "no configuration file was loaded"
    };
    messages.push(format!("{}, {}", target_info, config_info));

    let operator_info = match config.operator {
        Some(true) => "be used",
        Some(false) => "not be used",
        None => "be used if possible",
    };
    messages.push(format!("operator: the operator will {}", operator_info));

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
    messages.push(format!(
        "env: {} environment variables will be fetched",
        env_info
    ));

    let fs_info = match config.feature.fs.mode {
        FsModeConfig::Read => "read only from the remote",
        FsModeConfig::Write => "read and write from the remote",
        _ => "read and write locally",
    };
    messages.push(format!("fs: file operations will default to {}", fs_info));

    let incoming_info = match config.feature.network.incoming.mode {
        IncomingMode::Mirror => "mirrored",
        IncomingMode::Steal => "stolen",
        IncomingMode::Off => "ignored",
    };
    messages.push(format!(
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
        messages.push(format!("incoming: traffic will only be stolen from {filtered_port_str}{and}{unfiltered_ports_str}"));
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
    messages.push(format!("outgoing: forwarding is {}", outgoing_info));

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
    messages.push(format!("dns: DNS will be resolved {}", dns_info));

    if single_msg {
        let long_message = messages.join(". \n");
        progress_subtask.info(&long_message);
    } else {
        for m in messages {
            progress_subtask.info(&m[..]);
        }
    }
}

async fn exec(args: &ExecArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord exec");
    if !args.params.disable_version_check {
        prompt_outdated_version(&progress).await;
    }
    info!(
        "Launching {:?} with arguments {:?}",
        args.binary, args.binary_args
    );

    if !(args.params.no_tcp_outgoing || args.params.no_udp_outgoing) && args.params.no_remote_dns {
        warn!("TCP/UDP outgoing enabled without remote DNS might cause issues when local machine has IPv6 enabled but remote cluster doesn't")
    }

    for (name, value) in args.params.as_env_vars()? {
        std::env::set_var(name, value);
    }

    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, Default::default(), watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let execution_result = exec_process(config, args, &progress, &mut analytics).await;

    if execution_result.is_err() && !analytics.has_error() {
        analytics.set_error(AnalyticsError::Unknown);
    }

    execution_result
}

/// Lists targets based on whether or not the operator has been enabled in `layer_config`.
/// If the operator is enabled (and we can reach it), then we list [`Seeker::all`] targets, otherwise we list [`Seeker::all_open_source`] only.
async fn list_targets(layer_config: &LayerConfig, args: &ListTargetArgs) -> Result<Vec<String>> {
    let client = create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig.clone(),
        layer_config.kube_context.clone(),
    )
    .await
    .and_then(|config| Client::try_from(config).map_err(From::from))
    .map_err(|error| CliError::auth_exec_error_or(error, CliError::CreateKubeApiFailed))?;

    let namespace = args
        .namespace
        .as_deref()
        .or(layer_config.target.namespace.as_deref());

    let seeker = KubeResourceSeeker {
        client: &client,
        namespace,
    };

    let mut reporter = NullReporter::default();

    let operator_api = if layer_config.operator != Some(false)
        && let Some(api) = OperatorApi::try_new(layer_config, &mut reporter).await?
    {
        let api = api.prepare_client_cert(&mut reporter).await;

        api.inspect_cert_error(
            |error| tracing::error!(%error, "failed to prepare client certificate"),
        );

        Some(api)
    } else {
        None
    };

    match operator_api {
        None if layer_config.operator == Some(true) => Err(CliError::OperatorNotInstalled),
        Some(api)
            if ALL_TARGETS_SUPPORTED_OPERATOR_VERSION
                .matches(&api.operator().spec.operator_version) =>
        {
            seeker
                .all()
                .await
                .map_err(|error| CliError::auth_exec_error_or(error, CliError::ListTargetsFailed))
        }
        _ => seeker
            .all_open_source()
            .await
            .map_err(|error| CliError::auth_exec_error_or(error, CliError::ListTargetsFailed)),
    }
}

/// Lists all possible target paths.
/// Tries to use operator if available, otherwise falls back to k8s API (if operator isn't
/// explicitly true). Example:
/// ```
/// [
///  "pod/metalbear-deployment-85c754c75f-982p5",
///  "pod/nginx-deployment-66b6c48dd5-dc9wk",
///  "pod/py-serv-deployment-5c57fbdc98-pdbn4/container/py-serv",
///  "deployment/nginx-deployment"
///  "deployment/nginx-deployment/container/nginx"
///  "rollout/nginx-rollout"
///  "statefulset/nginx-statefulset"
///  "statefulset/nginx-statefulset/container/nginx"
/// ]
/// ```
async fn print_targets(args: &ListTargetArgs) -> Result<()> {
    let mut layer_config = if let Some(config) = &args.config_file {
        let mut cfg_context = ConfigContext::default();
        LayerFileConfig::from_path(config)?.generate_config(&mut cfg_context)?
    } else {
        LayerConfig::from_env()?
    };

    if let Some(namespace) = &args.namespace {
        layer_config.target.namespace = Some(namespace.clone());
    };

    if !layer_config.use_proxy {
        remove_proxy_env();
    }

    let mut targets = list_targets(&layer_config, args).await?;

    targets.sort();

    let json_obj = json!(targets);
    println!("{json_obj}");
    Ok(())
}

async fn port_forward(args: &PortForwardArgs, watch: drain::Watch) -> Result<()> {
    let mut progress = ProgressTracker::from_env("mirrord port-forward");
    progress.warning("Port forwarding is currently an unstable feature and subject to change. See https://github.com/metalbear-co/mirrord/issues/2640 for more info.");
    if !args.disable_version_check {
        prompt_outdated_version(&progress).await;
    }

    for (name, value) in args.target.as_env_vars()? {
        std::env::set_var(name, value);
    }

    if args.no_telemetry {
        std::env::set_var("MIRRORD_TELEMETRY", "false");
    }

    if let Some(namespace) = &args.agent_namespace {
        std::env::set_var("MIRRORD_AGENT_NAMESPACE", namespace.clone());
    }

    if let Some(log_level) = &args.agent_log_level {
        std::env::set_var("MIRRORD_AGENT_RUST_LOG", log_level.clone());
    }

    if let Some(image) = &args.agent_image {
        std::env::set_var("MIRRORD_AGENT_IMAGE", image.clone());
    }

    if let Some(agent_ttl) = &args.agent_ttl {
        std::env::set_var("MIRRORD_AGENT_TTL", agent_ttl.to_string());
    }
    if let Some(agent_startup_timeout) = &args.agent_startup_timeout {
        std::env::set_var(
            "MIRRORD_AGENT_STARTUP_TIMEOUT",
            agent_startup_timeout.to_string(),
        );
    }

    if args.accept_invalid_certificates {
        std::env::set_var("MIRRORD_ACCEPT_INVALID_CERTIFICATES", "true");
        warn!("Accepting invalid certificates");
    }

    if args.ephemeral_container {
        std::env::set_var("MIRRORD_EPHEMERAL_CONTAINER", "true");
    };

    if let Some(context) = &args.context {
        std::env::set_var("MIRRORD_KUBE_CONTEXT", context);
    }

    if let Some(config_file) = &args.config_file {
        std::env::set_var("MIRRORD_CONFIG_FILE", config_file);
    }

    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::new(config.telemetry, ExecutionKind::PortForward, watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let (_connection_info, connection) =
        create_and_connect(&config, &mut progress, &mut analytics).await?;
    let mut port_forward = PortForwarder::new(connection, args.port_mappings.clone()).await?;
    port_forward.run().await?;
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

    let res: Result<(), CliError> = rt.block_on(async move {
        if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
            mirrord_console::init_async_logger(&console_addr, watch.clone(), 124).await?;
        } else if !init_ext_error_handler(&cli.commands) {
            registry()
                .with(fmt::layer().with_writer(std::io::stderr))
                .with(EnvFilter::from_default_env())
                .init();
        }

        match cli.commands {
            Commands::Exec(args) => exec(&args, watch).await?,
            Commands::Extract { path } => {
                extract_library(
                    Some(path),
                    &ProgressTracker::from_env("mirrord extract library..."),
                    false,
                )?;
            }
            Commands::ListTargets(args) => print_targets(&args).await?,
            Commands::Operator(args) => operator_command(*args).await?,
            Commands::ExtensionExec(args) => {
                extension_exec(*args, watch).await?;
            }
            Commands::InternalProxy => internal_proxy::proxy(watch).await?,
            Commands::VerifyConfig(args) => verify_config(args).await?,
            Commands::Completions(args) => {
                let mut cmd: clap::Command = Cli::command();
                generate(args.shell, &mut cmd, "mirrord", &mut std::io::stdout());
            }
            Commands::Teams => teams::navigate_to_intro().await,
            Commands::Diagnose(args) => diagnose_command(*args).await?,
            Commands::Container(args) => {
                let (runtime_args, exec_params) = args.into_parts();
                container_command(runtime_args, exec_params, watch).await?
            }
            Commands::ExternalProxy => external_proxy::proxy(watch).await?,
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

// only ls and ext commands need the errors in json format
// error logs are disabled for extensions
fn init_ext_error_handler(commands: &Commands) -> bool {
    match commands {
        Commands::ListTargets(_) | Commands::ExtensionExec(_) => {
            let _ = miette::set_hook(Box::new(|_| Box::new(JSONReportHandler::new())));
            true
        }
        Commands::InternalProxy => true,
        _ => false,
    }
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
