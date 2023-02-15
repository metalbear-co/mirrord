#![feature(let_chains)]

use std::{collections::HashMap, time::Duration};

use clap::Parser;
use config::*;
use exec::execvp;
use execution::MirrordExecution;
use extension::extension_exec;
use extract::extract_library;
use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api};
use mirrord_auth::AuthConfig;
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{container::SKIP_NAMES, get_k8s_resource_api, kubernetes::create_kube_api},
    error::KubeApiError,
};
use mirrord_progress::{Progress, TaskProgress};
#[cfg(target_os = "macos")]
use mirrord_sip::sip_patch;
use operator::operator_command;
use semver::Version;
use serde_json::json;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

mod config;
mod connection;
mod error;
mod execution;
mod extension;
mod extract;
mod internal_proxy;
mod operator;

pub(crate) use error::{CliError, Result};

const PAUSE_WITHOUT_STEAL_WARNING: &str =
    "--pause specified without --steal: Incoming requests to the application will
                                           not be handled. The target container running the deployed application is paused,
                                           and responses from the local application are dropped.

                                           Attention: if network based liveness/readiness probes are defined for the
                                           target, they will fail under this configuration.

                                           To have the local application handle incoming requests you can run again with
                                           `--steal`. To have the deployed application handle requests, run again without
                                           specifying `--pause`.
    ";

async fn exec(args: &ExecArgs, progress: &TaskProgress) -> Result<()> {
    if !args.no_telemetry {
        prompt_outdated_version().await;
    }
    info!(
        "Launching {:?} with arguments {:?}",
        args.binary, args.binary_args
    );

    if !(args.no_tcp_outgoing || args.no_udp_outgoing) && args.no_remote_dns {
        warn!("TCP/UDP outgoing enabled without remote DNS might cause issues when local machine has IPv6 enabled but remote cluster doesn't")
    }

    if let Some(target) = &args.target {
        std::env::set_var("MIRRORD_IMPERSONATED_TARGET", target);
    }

    if let Some(skip_processes) = &args.skip_processes {
        std::env::set_var("MIRRORD_SKIP_PROCESSES", skip_processes.clone());
    }

    if let Some(namespace) = &args.target_namespace {
        std::env::set_var("MIRRORD_TARGET_NAMESPACE", namespace.clone());
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

    if let Some(fs_mode) = args.fs_mode {
        std::env::set_var("MIRRORD_FILE_MODE", fs_mode.to_string());
    }

    if let Some(override_env_vars_exclude) = &args.override_env_vars_exclude {
        std::env::set_var(
            "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE",
            override_env_vars_exclude,
        );
    }

    if let Some(override_env_vars_include) = &args.override_env_vars_include {
        std::env::set_var(
            "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE",
            override_env_vars_include,
        );
    }

    if args.no_remote_dns {
        std::env::set_var("MIRRORD_REMOTE_DNS", "false");
    }

    if args.accept_invalid_certificates {
        std::env::set_var("MIRRORD_ACCEPT_INVALID_CERTIFICATES", "true");
    }

    if args.ephemeral_container {
        std::env::set_var("MIRRORD_EPHEMERAL_CONTAINER", "true");
    };

    if args.tcp_steal {
        std::env::set_var("MIRRORD_AGENT_TCP_STEAL_TRAFFIC", "true");
    };

    if args.pause {
        std::env::set_var("MIRRORD_PAUSE", "true");
    }

    if args.no_outgoing || args.no_tcp_outgoing {
        std::env::set_var("MIRRORD_TCP_OUTGOING", "false");
    }

    if args.no_outgoing || args.no_udp_outgoing {
        std::env::set_var("MIRRORD_UDP_OUTGOING", "false");
    }

    if let Some(config_file) = &args.config_file {
        // Set canoncialized path to config file, in case forks/children are in different
        // working directories.
        let full_path = std::fs::canonicalize(config_file)
            .map_err(|e| CliError::ConfigFilePathError(config_file.to_owned(), e))?;
        std::env::set_var("MIRRORD_CONFIG_FILE", full_path);
    }

    if args.capture_error_trace {
        std::env::set_var("MIRRORD_CAPTURE_ERROR_TRACE", "true");
    }

    let sub_progress = progress.subtask("preparing to launch process");

    #[cfg(target_os = "macos")]
    let (_did_sip_patch, binary) = match sip_patch(&args.binary)? {
        None => (false, args.binary.clone()),
        Some(sip_result) => (true, sip_result),
    };

    #[cfg(not(target_os = "macos"))]
    let binary = args.binary.clone();

    let config = LayerConfig::from_env()?;
    if config.agent.pause {
        if config.agent.ephemeral {
            error!("Pausing is not yet supported together with an ephemeral agent container.");
            panic!("Mutually exclusive arguments `--pause` and `--ephemeral-container` passed together.");
        }
        if !config.feature.network.incoming.is_steal() {
            warn!("{PAUSE_WITHOUT_STEAL_WARNING}");
        }
    }

    let execution_info = MirrordExecution::start(&config, progress).await?;

    // Stop confusion with layer
    std::env::set_var(mirrord_progress::MIRRORD_PROGRESS_ENV, "off");

    // Set environment variables from agent + layer settings.
    for (key, value) in execution_info.environment {
        std::env::set_var(key, value);
    }

    let mut binary_args = args.binary_args.clone();
    binary_args.insert(0, args.binary.clone());

    sub_progress.done_with("ready to launch process");
    // The execve hook is not yet active and does not hijack this call.
    let err = execvp(binary.clone(), binary_args.clone());
    error!("Couldn't execute {:?}", err);
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if let exec::Error::Errno(errno::Errno(86)) = err {
        // "Bad CPU type in executable"
        if _did_sip_patch {
            return Err(CliError::RosettaMissing(binary));
        }
    }
    Err(CliError::BinaryExecuteFailed(binary, binary_args))
}

/// Returns a list of (pod name, [container names]) pairs.
/// Filtering mesh side cars
async fn get_kube_pods(namespace: Option<&str>) -> Result<HashMap<String, Vec<String>>> {
    let client = create_kube_api(None)
        .await
        .map_err(CliError::KubernetesApiFailed)?;
    let api: Api<Pod> = get_k8s_resource_api(&client, namespace);
    let pods = api
        .list(&ListParams::default())
        .await
        .map_err(KubeApiError::from)
        .map_err(CliError::KubernetesApiFailed)?;

    // convert pods to (name, container names) pairs

    let pod_containers_map: HashMap<String, Vec<String>> = pods
        .items
        .iter()
        .filter_map(|pod| {
            let name = pod.metadata.name.clone()?;
            let containers = pod
                .spec
                .as_ref()?
                .containers
                .iter()
                .filter_map(|container| {
                    // filter out mesh side cars
                    (!SKIP_NAMES.contains(container.name.as_str())).then(|| container.name.clone())
                })
                .collect();
            Some((name, containers))
        })
        .collect();

    Ok(pod_containers_map)
}

/// Lists all possible target paths for pods.
/// Example: ```[
///  "pod/metalbear-deployment-85c754c75f-982p5",
///  "pod/nginx-deployment-66b6c48dd5-dc9wk",
///  "pod/py-serv-deployment-5c57fbdc98-pdbn4/container/py-serv",
/// ]```
async fn print_pod_targets(args: &ListTargetArgs) -> Result<()> {
    let pods = get_kube_pods(args.namespace.as_deref()).await?;
    let target_vector = pods
        .iter()
        .flat_map(|(pod, containers)| {
            if containers.len() == 1 {
                vec![format!("pod/{pod}")]
            } else {
                containers
                    .iter()
                    .map(move |container| format!("pod/{pod}/container/{container}"))
                    .collect::<Vec<String>>()
            }
        })
        .collect::<Vec<String>>();
    let json_obj = json!(target_vector);
    println!("{json_obj}");
    Ok(())
}

fn login(args: LoginArgs) -> Result<()> {
    match &args.token {
        Some(token) => AuthConfig::from_input(token)?.save()?,
        None => {
            AuthConfig::from_webbrowser(&args.auth_server, args.timeout, args.no_open)?.save()?
        }
    }

    println!(
        "Config succesfuly saved at {}",
        AuthConfig::config_path().display()
    );

    Ok(())
}

fn cli_progress() -> TaskProgress {
    TaskProgress::new("mirrord cli starting")
}

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> miette::Result<()> {
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_logger(&console_addr).await?;
    } else {
        registry()
            .with(fmt::layer().with_writer(std::io::stderr))
            .with(EnvFilter::from_default_env())
            .init();
    }

    let cli = Cli::parse();

    match cli.commands {
        Commands::Exec(args) => exec(&args, &cli_progress()).await?,
        Commands::Extract { path } => {
            extract_library(Some(path), &cli_progress(), false)?;
        }
        Commands::ListTargets(args) => print_pod_targets(&args).await?,
        Commands::Login(args) => login(args)?,
        Commands::Operator(args) => operator_command(*args).await?,
        Commands::ExtensionExec(args) => extension_exec(*args).await?,
        Commands::InternalProxy => internal_proxy::proxy().await?,
    }

    Ok(())
}

async fn prompt_outdated_version() {
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
                        println!("New mirrord version available: {latest_version}. To update, run: `curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash`.");
                        println!("To disable version checks, set env variable MIRRORD_CHECK_VERSION to 'false'.")
                    }
                }
            }
        }
    }
}
