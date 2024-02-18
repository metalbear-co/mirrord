#![feature(let_chains)]
#![feature(lazy_cell)]
#![warn(clippy::indexing_slicing)]

use std::{collections::HashMap, time::Duration};

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use config::*;
use exec::execvp;
use execution::MirrordExecution;
use extension::extension_exec;
use extract::extract_library;
use k8s_openapi::{
    api::{apps::v1::Deployment, core::v1::Pod},
    Metadata, NamespaceResourceScope,
};
use kube::api::ListParams;
use miette::JSONReportHandler;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics};
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_kube::{
    api::{
        container::SKIP_NAMES,
        kubernetes::{create_kube_api, get_k8s_resource_api, rollout::Rollout},
    },
    error::KubeApiError,
};
use mirrord_progress::{Progress, ProgressTracker};
use operator::operator_command;
use semver::Version;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};
use which::which;

mod config;
mod connection;
mod error;
mod execution;
mod extension;
mod extract;
mod internal_proxy;
mod operator;
mod teams;
mod verify_config;

pub(crate) use error::{CliError, Result};
use verify_config::verify_config;

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
        Some(sip_result) => (true, sip_result),
    };

    #[cfg(not(target_os = "macos"))]
    let binary = args.binary.clone();

    // Stop confusion with layer
    std::env::set_var(mirrord_progress::MIRRORD_PROGRESS_ENV, "off");

    // Set environment variables from agent + layer settings.
    for (key, value) in &execution_info.environment {
        std::env::set_var(key, value);
    }

    let mut binary_args = args.binary_args.clone();
    // Put original executable in argv[0] even if actually running patched version.
    binary_args.insert(0, args.binary.clone());

    sub_progress.success(Some("ready to launch process"));
    // The execve hook is not yet active and does not hijack this call.
    let err = execvp(binary.clone(), binary_args.clone());
    error!("Couldn't execute {:?}", err);
    analytics.set_error(AnalyticsError::BinaryExecuteFailed);
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

async fn exec(args: &ExecArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord exec");
    if !args.disable_version_check {
        prompt_outdated_version(&progress).await;
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

    if args.no_telemetry {
        std::env::set_var("MIRRORD_TELEMETRY", "false");
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
        warn!("Accepting invalid certificates");
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

    if let Some(context) = &args.context {
        std::env::set_var("MIRRORD_KUBE_CONTEXT", context);
    }

    if let Some(config_file) = &args.config_file {
        // Set canoncialized path to config file, in case forks/children are in different
        // working directories.
        let full_path = std::fs::canonicalize(config_file)
            .map_err(|e| CliError::ConfigFilePathError(config_file.to_owned(), e))?;
        std::env::set_var("MIRRORD_CONFIG_FILE", full_path);
    }

    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, watch);
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

/// Returns a list of (pod name, [container names]) pairs, filtering out mesh side cars
/// as well as any pods which are not ready or have crashed.
async fn get_kube_pods(
    namespace: Option<&str>,
    client: &kube::Client,
) -> Result<HashMap<String, Vec<String>>> {
    let pods = get_kube_resources::<Pod>(namespace, client, Some("status.phase=Running"))
        .await
        .filter(|pod| {
            pod.status
                .as_ref()
                .and_then(|status| status.conditions.as_ref())
                .map(|conditions| {
                    // filter out pods without the Ready condition
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
                })
                .unwrap_or(false)
        });

    // convert pods to (name, container names) pairs
    let pod_containers_map: HashMap<String, Vec<String>> = pods
        .filter_map(|pod| {
            let name = pod.metadata.name.clone()?;
            let containers = pod
                .spec
                .as_ref()?
                .containers
                .iter()
                .filter(|&container| (!SKIP_NAMES.contains(container.name.as_str())))
                .map(|container| container.name.clone())
                .collect();
            Some((name, containers))
        })
        .collect();

    Ok(pod_containers_map)
}

async fn get_kube_deployments(
    namespace: Option<&str>,
    client: &kube::Client,
) -> Result<impl Iterator<Item = String>> {
    Ok(get_kube_resources::<Deployment>(namespace, client, None)
        .await
        .filter(|deployment| {
            deployment
                .status
                .as_ref()
                .map(|status| status.available_replicas >= Some(1))
                .unwrap_or(false)
        })
        .filter_map(|deployment| deployment.metadata.name))
}

async fn get_kube_rollouts(
    namespace: Option<&str>,
    client: &kube::Client,
) -> Result<impl Iterator<Item = String>> {
    Ok(get_kube_resources::<Rollout>(namespace, client, None)
        .await
        .filter_map(|rollout| rollout.metadata().name.clone()))
}

async fn get_kube_resources<K>(
    namespace: Option<&str>,
    client: &kube::Client,
    field_selector: Option<&str>,
) -> impl Iterator<Item = K>
where
    K: kube::Resource<Scope = NamespaceResourceScope>,
    <K as kube::Resource>::DynamicType: Default,
    K: Clone + DeserializeOwned + std::fmt::Debug,
{
    // Set up filters on the K8s resources returned - in this case, excluding the agent resources
    // and then applying any provided field-based filter conditions.
    let params = &mut ListParams::default().labels("app!=mirrord");
    if let Some(fields) = field_selector {
        params.field_selector = Some(fields.to_string())
    }
    get_k8s_resource_api(client, namespace)
        .list(params)
        .await
        .map(|resources| resources.into_iter())
        .map_err(KubeApiError::from)
        .map_err(CliError::KubernetesApiFailed)
        .unwrap_or_else(|_| Vec::new().into_iter())
}

/// Lists all possible target paths for pods.
/// Example: ```[
///  "pod/metalbear-deployment-85c754c75f-982p5",
///  "pod/nginx-deployment-66b6c48dd5-dc9wk",
///  "pod/py-serv-deployment-5c57fbdc98-pdbn4/container/py-serv",
/// ]```
async fn print_pod_targets(args: &ListTargetArgs) -> Result<()> {
    let (accept_invalid_certificates, kubeconfig, namespace, kube_context) = if let Some(config) =
        &args.config_file
    {
        let mut cfg_context = ConfigContext::default();
        let layer_config = LayerFileConfig::from_path(config)?.generate_config(&mut cfg_context)?;
        (
            layer_config.accept_invalid_certificates,
            layer_config.kubeconfig,
            layer_config.target.namespace,
            layer_config.kube_context,
        )
    } else {
        (false, None, None, None)
    };

    let client = create_kube_api(accept_invalid_certificates, kubeconfig, kube_context)
        .await
        .map_err(CliError::KubernetesApiFailed)?;

    let namespace = args.namespace.as_deref().or(namespace.as_deref());

    let (pods, deployments, rollouts) = futures::try_join!(
        get_kube_pods(namespace, &client),
        get_kube_deployments(namespace, &client),
        get_kube_rollouts(namespace, &client)
    )?;

    let mut target_vector = pods
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
        .chain(deployments.map(|deployment| format!("deployment/{deployment}")))
        .chain(rollouts.map(|rollout| format!("rollout/{rollout}")))
        .collect::<Vec<String>>();

    target_vector.sort();

    let json_obj = json!(target_vector);
    println!("{json_obj}");
    Ok(())
}

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> miette::Result<()> {
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
            Commands::ListTargets(args) => print_pod_targets(&args).await?,
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
                        progress.success(Some(&format!("Update to {latest_version} available")));
                    } else {
                        progress.success(Some(&format!("Running on latest ({CURRENT_VERSION})!")));
                    }
                }
            }
        }
    }
}
