use std::sync::LazyLock;

use futures::TryStreamExt;
use k8s_openapi::api::core::v1::Namespace;
use kube::Client;
use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_kube::{
    api::kubernetes::{create_kube_config, list, seeker::KubeResourceSeeker},
    error::KubeApiError,
};
use mirrord_operator::client::OperatorApi;
use semver::VersionReq;
use serde::{ser::SerializeSeq, Serialize, Serializer};

use crate::{util, CliError, CliResult, Format, ListTargetArgs};

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
    /// If `fetch_namespaces` is set, returned [`FoundTargets`] will contain info about namespaces
    /// available in the cluster.
    async fn resolve(config: LayerConfig, fetch_namespaces: bool) -> CliResult<Self> {
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

        let mut reporter = NullReporter::default();
        let operator_api = if config.operator != Some(false)
            && let Some(api) = OperatorApi::try_new(&config, &mut reporter).await?
        {
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
            namespace: config.target.namespace.as_deref(),
        };
        let paths = match operator_api {
            None if config.operator == Some(true) => Err(CliError::OperatorNotInstalled),

            Some(api)
                if ALL_TARGETS_SUPPORTED_OPERATOR_VERSION
                    .matches(&api.operator().spec.operator_version) =>
            {
                seeker.all().await.map_err(|error| {
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
            .collect();
        let current_namespace = config
            .target
            .namespace
            .as_deref()
            .unwrap_or(client.default_namespace())
            .to_owned();

        let namespaces = if fetch_namespaces {
            list::list_all_clusterwide::<Namespace>(client, None)
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

        Ok(Self {
            targets,
            current_namespace,
            namespaces,
        })
    }
}

/// Thin wrapper over [`FoundTargets`] that implements [`Serialize`].
/// Its serialized format is a sequence of available target paths.
struct SimpleDisplay<'a>(&'a FoundTargets);

impl Serialize for SimpleDisplay<'_> {
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
    LazyLock::new(|| ">=3.84.0".parse().expect("verion should be valid"));

/// Fetches mirrord targets from the cluster and prints output to stdout.
pub async fn print_targets(args: ListTargetArgs, rich_output: bool) -> CliResult<()> {
    let mut layer_config = if let Some(config) = &args.config_file {
        let mut cfg_context = ConfigContext::default();
        LayerFileConfig::from_path(config)?.generate_config(&mut cfg_context)?
    } else {
        LayerConfig::from_env()?
    };

    if let Some(namespace) = args.namespace {
        layer_config.target.namespace.replace(namespace);
    };

    if !layer_config.use_proxy {
        util::remove_proxy_env();
    }

    let targets = FoundTargets::resolve(layer_config, rich_output).await?;

    match args.output {
        Format::Json => {
            let serialized = if rich_output {
                serde_json::to_string(&targets).unwrap()
            } else {
                serde_json::to_string(&SimpleDisplay(&targets)).unwrap()
            };

            println!("{serialized}");
        }
    }

    Ok(())
}
