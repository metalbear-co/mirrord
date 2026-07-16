//! Implementation of the `mirrord up` command, which runs multiple mirrord sessions
//! from a single configuration file.

#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
#![deny(missing_docs)]

use std::{
    collections::HashMap,
    ops::Not,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    str::FromStr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use config::{ResolvedTarget, SpecifiedTarget, UnresolvedTarget};
use futures::TryStreamExt;
use inquire::{Confirm, Select};
use k8s_openapi::api::core::v1::Namespace;
use miette::Diagnostic;
use mirrord_analytics::MIRRORD_UP_CORRELATION_ID_ENV;
use mirrord_config::{
    config::{ConfigError, EnvKey},
    target::{Target, TargetType},
};
use mirrord_kube::{
    api::kubernetes::{create_kube_config, seeker::KubeResourceSeeker},
    error::KubeApiError,
};
use thiserror::Error;
use yamlpatch::{Op, Patch, apply_yaml_patches};
use yamlpath::{Document, route};
mod config;
mod init;

pub use config::{ServiceMode, SubprocessCfg, UpConfig};
pub use init::{InitError, run_wizard};
use mirrord_progress::{MIRRORD_PROGRESS_ENV, messages::SESSION_READY_MESSAGE};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    task::{JoinError, JoinSet},
};
use uuid::Uuid;

/// Shared slot that [`run`] fills with the time it took for **all** child
/// sessions to become ready, measured from process spawn.
///
/// The caller constructs one, passes a clone to [`run`], and reads
/// [`ReadyTracker::time_to_ready`] afterwards (the value is captured even if
/// `run` later returns an error, as long as readiness was reached first).
#[derive(Clone, Default)]
pub struct ReadyTracker {
    elapsed: Arc<OnceLock<Duration>>,
}

impl ReadyTracker {
    /// The time from spawn until every session became ready, if that happened.
    pub fn time_to_ready(&self) -> Option<Duration> {
        self.elapsed.get().copied()
    }
}

/// Environment variable used to pass the resolved configuration to child mirrord processes.
pub const RESOLVED_CONFIG_ENV: &str = "MIRRORD_UP_RESOLVED_CONFIG";

/// Errors produced by `mirrord up` command.
#[derive(Debug, Error, Diagnostic)]
pub enum UpError {
    /// IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Failed to parse the mirrord-up YAML configuration.
    #[error("failed to parse mirrord-up config: {0}")]
    #[diagnostic(help("Check the YAML syntax and field names in your mirrord-up.yaml."))]
    Parse(#[from] serde_yaml::Error),

    /// Configuration validation failed.
    #[error("mirrord-up config validation failed: {0}")]
    Validation(#[from] ConfigError),

    /// A child mirrord service exited with a non-zero status.
    #[error("Service {name} crashed with exit status {status}")]
    ServiceCrashed {
        /// Name of the service that crashed.
        name: Arc<str>,
        /// Exit status of the crashed service.
        status: ExitStatus,
    },

    /// A child process handler task panicked.
    #[error("Child process handler task panicked: {0:?}")]
    Panic(JoinError),

    /// Failed to build a Kubernetes client or query the cluster while
    /// resolving service targets.
    #[error("failed to query the cluster: {0}")]
    Kube(#[from] KubeApiError),

    /// An interactive target-resolution prompt failed or was cancelled.
    #[error("target resolution prompt failed: {0}")]
    Inquire(#[from] inquire::InquireError),

    /// The user chose to exit from the target-resolution wizard.
    #[error("exited")]
    Exited,

    /// Failed to patch the `mirrord-up.yaml` when saving a chosen target.
    #[error("failed to update config file: {0}")]
    YamlPatch(#[from] yamlpatch::Error),

    /// Failed to parse the `mirrord-up.yaml` when saving a chosen target.
    #[error("failed to parse config file for updating: {0}")]
    YamlQuery(#[from] yamlpath::QueryError),
}

/// Load and parse a `mirrord-up.yaml` configuration file.
pub fn load_up_config(path: &PathBuf) -> Result<UpConfig, UpError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&content)?)
}

/// Workload kinds considered when inferring a target from a service key, in
/// match-priority order: when several kinds have a workload with the same
/// name, the first kind here wins.
const INFERRED_TARGET_TYPES: [TargetType; 4] = [
    TargetType::Deployment,
    TargetType::StatefulSet,
    TargetType::Rollout,
    TargetType::Pod,
];

/// Resolve every service whose `target.path` is absent by looking the service
/// key up in the cluster (see [`INFERRED_TARGET_TYPES`] for the search order).
/// When no workload matches, an interactive wizard lets the user pick a
/// different namespace and workload, or exit.
async fn resolve_unresolved_workloads(
    up_config: &UpConfig,
    config_path: &Path,
) -> Result<HashMap<UnresolvedTarget, ResolvedTarget>, UpError> {
    let unresolved = up_config.unresolved_targets();

    let (_, bound_max) = unresolved.size_hint();

    // No unresolved
    if bound_max == Some(0) {
        return Ok(HashMap::new());
    }

    let kube_config = create_kube_config(
        up_config.common.accept_invalid_certificates,
        None::<&str>,
        None,
    )
    .await?;
    let client = kube::Client::try_from(kube_config).map_err(KubeApiError::from)?;

    let mut map = HashMap::new();
    for unresolved_target in unresolved {
        let name = &unresolved_target.workload_name;
        let namespace = unresolved_target
            .namespace
            .as_deref()
            .unwrap_or(client.default_namespace());

        let workloads = list_workloads(&client, namespace).await?;
        let resolved = match find_by_name(&workloads, name) {
            Some(path) => {
                println!("{name}: using {path}");
                ResolvedTarget {
                    resolved: SpecifiedTarget {
                        path: Some(Target::from_str(path)?),
                        namespace: unresolved_target.namespace.clone(),
                    },
                }
            }
            None => {
                println!(
                    "{name}: No workload named \"{name}\" found in namespace \"{namespace}\"."
                );
                prompt_for_target(&client, name, config_path).await?
            }
        };

        map.insert(unresolved_target, resolved);
    }

    Ok(map)
}

/// Lists workload paths (`deployment/foo`, `pod/bar/container/baz`, ...) of
/// the kinds in [`INFERRED_TARGET_TYPES`] living in `namespace`, in priority
/// order.
async fn list_workloads(client: &kube::Client, namespace: &str) -> Result<Vec<String>, UpError> {
    let seeker = KubeResourceSeeker {
        client,
        namespace,
        copy_target: false,
    };

    // `operator_active: true` only lifts the seeker's restriction on listing
    // operator-only kinds (statefulsets); the listing itself is plain
    // Kubernetes API access.
    Ok(seeker
        .filtered(INFERRED_TARGET_TYPES.to_vec(), true)
        .await?)
}

async fn list_namespaces(client: &kube::Client) -> Result<Vec<String>, UpError> {
    let seeker = KubeResourceSeeker {
        client,
        namespace: client.default_namespace(),
        copy_target: false,
    };

    let namespaces = seeker
        .list_all_clusterwide::<Namespace>(None)
        .try_filter_map(|namespace| std::future::ready(Ok(namespace.metadata.name)))
        .try_collect()
        .await
        .map_err(KubeApiError::KubeError)?;
    Ok(namespaces)
}

/// First path in `workloads` whose workload name matches `name` (paths look
/// like `kind/name[/container/c]`, so the name is the second segment).
fn find_by_name<'w>(workloads: &'w [String], name: &str) -> Option<&'w str> {
    workloads
        .iter()
        .map(String::as_str)
        .find(|path| path.split('/').nth(1) == Some(name))
}

/// Runs a blocking inquire prompt on the blocking thread pool.
///
/// The CLI drives this crate on a current-thread tokio runtime, so prompting
/// directly from an async fn would freeze the entire runtime (including the
/// kube client's background connection tasks) for as long as the user is
/// deciding.
async fn prompt<T: Send + 'static>(
    prompt: impl FnOnce() -> Result<T, inquire::InquireError> + Send + 'static,
) -> Result<T, UpError> {
    tokio::task::spawn_blocking(prompt)
        .await
        .map_err(UpError::Panic)?
        .map_err(UpError::from)
}

/// Fallback wizard for a service key with no matching workload: pick a
/// namespace and workload from the cluster, or exit.
///
/// Once a workload is chosen the user is offered to persist it back to
/// `config_path` (as `services.<service_name>.target`), so the next run uses it
/// directly without asking again.
async fn prompt_for_target(
    client: &kube::Client,
    service_name: &str,
    config_path: &Path,
) -> Result<ResolvedTarget, UpError> {
    const SEARCH: &str = "Search in a different namespace";
    const EXIT: &str = "Exit";

    let choice =
        prompt(|| Select::new("What would you like to do?", vec![SEARCH, EXIT]).prompt()).await?;

    if choice == EXIT {
        return Err(UpError::Exited);
    }

    let namespaces = list_namespaces(client).await?;
    loop {
        let namespace = {
            let namespaces = namespaces.clone();
            prompt(move || Select::new("Select a namespace:", namespaces).prompt()).await?
        };

        let workloads = list_workloads(client, &namespace).await?;
        if workloads.is_empty() {
            println!("No workloads found in namespace \"{namespace}\".");
            continue;
        }

        let path = {
            let message = format!("Select a workload in \"{namespace}\":");
            prompt(move || Select::new(&message, workloads).prompt()).await?
        };

        offer_to_save_target(config_path, service_name, &path, &namespace).await?;

        return Ok(ResolvedTarget {
            resolved: SpecifiedTarget {
                path: Some(Target::from_str(&path)?),
                namespace: Some(namespace.into()),
            },
        });
    }
}

/// Ask whether to persist the chosen target (`path` + `namespace`) to the
/// service's `target` in the config, and do so if the user agrees.
///
/// Writing back is best-effort: a patch failure is reported but does not abort
/// the run, since the target has already been resolved in memory.
async fn offer_to_save_target(
    config_path: &Path,
    service_name: &str,
    path: &str,
    namespace: &str,
) -> Result<(), UpError> {
    let message = format!(
        "Save target \"{path}\" in namespace \"{namespace}\" to {} for next time?",
        config_path.display()
    );
    let save = prompt(move || Confirm::new(&message).with_default(true).prompt()).await?;
    if save.not() {
        return Ok(());
    }

    match save_target(config_path, service_name, path, namespace) {
        Ok(()) => println!("Saved target to {}.", config_path.display()),
        Err(error) => eprintln!("Failed to save target to config: {error}"),
    }

    Ok(())
}

/// Sets `services.<service_name>.target` to the chosen `path` and `namespace`
/// in the config file, creating the `target` mapping if it is absent.
///
/// Uses `yamlpatch` so the rest of the document -- comments, key order,
/// formatting -- is preserved instead of being rewritten by a full re-serialize.
fn save_target(
    config_path: &Path,
    service_name: &str,
    path: &str,
    namespace: &str,
) -> Result<(), UpError> {
    let document = Document::new(std::fs::read_to_string(config_path)?)?;

    let patch = Patch {
        route: route!("services", service_name),
        operation: Op::MergeInto {
            key: "target".to_owned(),
            updates: [
                (
                    "path".to_owned(),
                    serde_yaml::Value::String(path.to_owned()),
                ),
                (
                    "namespace".to_owned(),
                    serde_yaml::Value::String(namespace.to_owned()),
                ),
            ]
            .into_iter()
            .collect(),
        },
    };

    let patched = apply_yaml_patches(&document, &[patch])?;
    std::fs::write(config_path, patched.source())?;
    Ok(())
}

/// Genererate [`mirrord_config::LayerConfig`]s based on the provided [`UpConfig`] and
/// [`EnvKey`] and spawn child mirrord processes. Stdout/stderr from
/// children will be printed to the console, prefixed with the name of
/// the session.
///
/// `ready` is filled with the time-to-ready once every session has signalled
/// readiness (see [`ReadyTracker`]).
///
/// Returns when one of the child mirrord sessions exits.
pub async fn run(
    up_config: UpConfig,
    config_path: &Path,
    key: EnvKey,
    correlation_id: Uuid,
    ready: ReadyTracker,
) -> Result<(), UpError> {
    let mut resolved_targets = resolve_unresolved_workloads(&up_config, config_path).await?;

    let commands: Vec<_> = up_config
        .service_configs(&key, &mut resolved_targets)
        .map(|config| {
            let SubprocessCfg {
                config,
                service_name,
                run,
            } = config;

            let encoded_cfg = config.encode()?;

            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.env(RESOLVED_CONFIG_ENV, encoded_cfg)
                .env(MIRRORD_PROGRESS_ENV, "simple")
                .env(MIRRORD_UP_CORRELATION_ID_ENV, correlation_id.to_string())
                .arg(Into::<&'static str>::into(run.r#type))
                .arg("--")
                .args(run.command)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            Ok((service_name, cmd))
        })
        .collect::<Result<_, UpError>>()?;

    let start = Instant::now();
    let total = commands.len();
    let ready_count = Arc::new(AtomicUsize::new(0));

    let mut handles = JoinSet::new();
    for (name, mut command) in commands {
        let mut child = command.spawn().unwrap();
        let ready_count = Arc::clone(&ready_count);
        let ready = ready.clone();
        handles.spawn(async move {
            let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
            let mut out = BufReader::new(child.stdout.take().unwrap()).lines();

            // A session is only counted once: the user's binary inherits the
            // same stdout after `execve`, so a later line matching the marker
            // must not be double-counted.
            let mut counted = false;

            loop {
                tokio::select! {
                    line = out.next_line() => match line {
                        Ok(Some(line)) => {
                            if counted.not() && line.trim() == SESSION_READY_MESSAGE {
                                counted = true;
                                if ready_count.fetch_add(1, Ordering::Relaxed) + 1 == total {
                                    // TODO(areg) downgrade to a
                                    // `debug_assert` once the feature
                                    // stabilizes.
                                    ready.elapsed.set(start.elapsed()).expect("only the final task should set the ready marker");
                                }
                            }
                            println!("{name}: {line}");
                        }
                        Ok(None) => {}
                        Err(err) => println!("{name} error: {err:?}"),
                    },

                    line = err.next_line() => match line {
                        Ok(Some(line)) => println!("{name}: {line}"),
                        Ok(None) => {}
                        Err(err) => println!("{name} error: {err:?}"),
                    },

                    status = child.wait() => {
                        let status = status?;
                        if status.success() {
                            break Ok(());
                        } else {
                            break Err(UpError::ServiceCrashed {
                                name,
                                status,
                            })
                        }
                    }
                }
            }
        });
    }

    let Some(status) = handles.join_next().await else {
        unreachable!("should have at least one service")
    };

    // Handle JoinError and UpError from child handler tasks
    status.map_err(UpError::Panic)??;

    Ok(())
}
