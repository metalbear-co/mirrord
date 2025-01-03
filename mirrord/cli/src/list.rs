use std::sync::LazyLock;

use kube::Client;
use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_kube::api::kubernetes::{create_kube_config, seeker::KubeResourceSeeker};
use mirrord_operator::client::OperatorApi;
use semver::VersionReq;
use serde::{ser::SerializeSeq, Serialize, Serializer};

use crate::{util, CliError, CliResult, Format, ListTargetArgs};

#[derive(Serialize)]
struct FoundTarget {
    path: String,
    available: bool,
}

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
    current_namespace: String,
    namespaces: Vec<String>,
}

impl FoundTargets {
    async fn resolve(config: LayerConfig) -> CliResult<Self> {
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

        Ok(Self {
            targets,
            current_namespace,
            namespaces: Default::default(),
        })
    }
}

struct SimpleDisplay<'a>(&'a FoundTargets);

impl<'a> Serialize for SimpleDisplay<'a> {
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

pub async fn print_targets(args: ListTargetArgs) -> CliResult<()> {
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

    let targets = FoundTargets::resolve(layer_config).await?;

    match args.output {
        Format::Json => {
            let serialized = if args.rich_output {
                serde_json::to_string(&targets).unwrap()
            } else {
                serde_json::to_string(&SimpleDisplay(&targets)).unwrap()
            };

            println!("{serialized}");
        }
    }

    Ok(())
}
