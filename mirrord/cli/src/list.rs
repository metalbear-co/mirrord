use std::{str::FromStr, sync::LazyLock, time::Instant};

use futures::TryStreamExt;
use k8s_openapi::api::core::v1::Namespace;
use kube::Client;
use mirrord_analytics::NullReporter;
use mirrord_config::{config::ConfigContext, target::TargetType, LayerConfig};
use mirrord_kube::{
    api::kubernetes::{create_kube_config, seeker::KubeResourceSeeker},
    error::KubeApiError,
};
use mirrord_operator::client::OperatorApi;
use semver::VersionReq;
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::Value;
use tracing::Level;

use crate::{util, CliError, CliResult, Format, ListTargetArgs};

const LS_TARGET_TYPES_ENV: &str = "MIRRORD_LS_TARGET_TYPES";

/// A mirrord target found in the cluster.
#[derive(Serialize)]
struct FoundTarget {
    /// E.g `pod/my-pod-1234/container/my-container`.
    path: String,

    /// Whether this target is currently available.
    ///
    /// # Note
    ///
    /// Right now this is always true. Some preliminary checks are done in the
    /// [`KubeResourceSeeker`] and results come filtered.
    ///
    /// This field is here for forward compatibility, because in the future we might want to return
    /// unavailable targets as well (along with some validation error message) to improve UX.
    available: bool,
}

/// Result of mirrord targets lookup in the cluster.
#[derive(Serialize)]
struct FoundTargets {
    /// In order:
    /// 1. deployments
    /// 2. rollouts
    /// 3. statefulsets
    /// 4. cronjobs
    /// 5. jobs
    /// 6. pods
    targets: Vec<FoundTarget>,

    /// Current lookup namespace.
    ///
    /// Taken from [`LayerConfig::target`], defaults to [`Client`]'s default namespace.
    current_namespace: String,

    /// Available lookup namespaces.
    namespaces: Vec<String>,
}

impl FoundTargets {
    /// Performs a lookup of mirrord targets in the cluster.
    ///
    /// Unless the operator is explicitly disabled, attempts to connect with it.
    /// Operator lookup affects returned results (e.g some targets are only available via the
    /// operator).
    ///
    /// If `rich_output` is set:
    /// 1. returned [`FoundTargets`] will contain info about namespaces available in the cluster;
    /// 2. only deployment, rollout, and pod targets will be fetched.
    #[tracing::instrument(level = Level::DEBUG, skip(config), name = "resolve_targets", err)]
    async fn resolve(
        config: LayerConfig,
        rich_output: bool,
        target_types: Option<Vec<TargetType>>,
    ) -> CliResult<Self> {
        let client = create_kube_config(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await
        .and_then(|config| Client::try_from(config).map_err(From::from))
        .map_err(|error| {
            CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed)
        })?;

        let start = Instant::now();
        let mut reporter = NullReporter::default();
        let operator_api = if config.operator != Some(false)
            && let Some(api) = OperatorApi::try_new(&config, &mut reporter).await?
        {
            tracing::debug!(elapsed_s = start.elapsed().as_secs_f32(), "Operator found");

            let api = api.prepare_client_cert(&mut reporter).await;

            api.inspect_cert_error(
                |error| tracing::error!(%error, "failed to prepare client certificate"),
            );

            Some(api)
        } else {
            None
        };

        let seeker = KubeResourceSeeker {
            client: &client,
            namespace: config
                .target
                .namespace
                .as_deref()
                .unwrap_or(client.default_namespace()),
        };

        let (targets, namespaces) = tokio::try_join!(
            async {
                let paths = match (operator_api, target_types) {
                    (None, _) if config.operator == Some(true) => {
                        Err(CliError::OperatorNotInstalled)
                    }

                    (Some(api), None)
                        if !rich_output
                            && ALL_TARGETS_SUPPORTED_OPERATOR_VERSION
                                .matches(&api.operator().spec.operator_version) =>
                    {
                        seeker.all().await.map_err(|error| {
                            CliError::friendlier_error_or_else(error, CliError::ListTargetsFailed)
                        })
                    }

                    (Some(api), Some(target_types))
                        if !rich_output
                            && ALL_TARGETS_SUPPORTED_OPERATOR_VERSION
                                .matches(&api.operator().spec.operator_version) =>
                    {
                        seeker.filtered(target_types, true).await.map_err(|error| {
                            CliError::friendlier_error_or_else(error, CliError::ListTargetsFailed)
                        })
                    }

                    (None, Some(target_types)) => {
                        seeker.filtered(target_types, false).await.map_err(|error| {
                            CliError::friendlier_error_or_else(error, CliError::ListTargetsFailed)
                        })
                    }

                    _ => seeker.all_open_source().await.map_err(|error| {
                        CliError::friendlier_error_or_else(error, CliError::ListTargetsFailed)
                    }),
                }?;

                let targets = paths
                    .into_iter()
                    .map(|path| FoundTarget {
                        path,
                        available: true,
                    })
                    .collect::<Vec<_>>();

                Ok::<_, CliError>(targets)
            },
            async {
                let namespaces = if rich_output {
                    seeker
                        .list_all_clusterwide::<Namespace>(None)
                        .try_filter_map(|namespace| std::future::ready(Ok(namespace.metadata.name)))
                        .try_collect::<Vec<_>>()
                        .await
                        .map_err(KubeApiError::KubeError)
                        .map_err(|error| {
                            CliError::friendlier_error_or_else(error, CliError::ListTargetsFailed)
                        })?
                } else {
                    Default::default()
                };

                Ok::<_, CliError>(namespaces)
            }
        )?;

        let current_namespace = config
            .target
            .namespace
            .as_deref()
            .unwrap_or(client.default_namespace())
            .to_owned();

        Ok(Self {
            targets,
            current_namespace,
            namespaces,
        })
    }
}

/// Thin wrapper over [`FoundTargets`] that implements [`Serialize`].
/// Its serialized format is a sequence of available target paths.
///
/// Used to print available targets when the plugin/extension does not support the full format
/// (backward compatibility).
struct FoundTargetsList<'a>(&'a FoundTargets);

impl Serialize for FoundTargetsList<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let count = self.0.targets.iter().filter(|t| t.available).count();
        let mut list = serializer.serialize_seq(Some(count))?;

        for target in self.0.targets.iter().filter(|t| t.available) {
            list.serialize_element(&target.path)?;
        }

        list.end()
    }
}

/// Controls whether we support listing all targets or just the open source ones.
static ALL_TARGETS_SUPPORTED_OPERATOR_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=3.84.0".parse().expect("version should be valid"));

/// Fetches mirrord targets from the cluster and prints output to stdout.
///
/// When `rich_output` is set:
/// 1. targets info is printed as a JSON object containing extra data;
/// 2. only deployment, rollout, and pod targets are fetched.
///
/// Otherwise:
/// 1. targets are printed as a plain JSON array of strings (backward compatibility);
/// 2. all available target types are fetched.
pub(super) async fn print_targets(args: ListTargetArgs, rich_output: bool) -> CliResult<()> {
    let mut cfg_config =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file);

    let mut layer_config = LayerConfig::resolve(&mut cfg_config)?;

    if let Some(namespace) = args.namespace {
        layer_config.target.namespace.replace(namespace);
    };

    if !layer_config.use_proxy {
        util::remove_proxy_env();
    }

    let target_types = if let Some(target_type) = args.target_type {
        Some(vec![target_type])
    } else {
        match std::env::var(LS_TARGET_TYPES_ENV)
            .ok()
            .map(|val| serde_json::from_str::<Vec<Value>>(val.as_ref()))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(|str| str.to_string()))
            .filter_map(|string| TargetType::from_str(&string).ok())
            .collect::<Vec<TargetType>>()
        {
            vec if vec.is_empty() => None,
            vec => Some(vec),
        }
    };

    let targets = FoundTargets::resolve(layer_config, rich_output, target_types).await?;

    match args.output {
        Format::Json => {
            let serialized = if rich_output {
                serde_json::to_string(&targets).unwrap()
            } else {
                serde_json::to_string(&FoundTargetsList(&targets)).unwrap()
            };

            println!("{serialized}");
        }
    }

    Ok(())
}
