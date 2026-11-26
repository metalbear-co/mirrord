use std::{ops::Not, sync::LazyLock, time::Instant};

use futures::TryStreamExt;
use k8s_openapi::api::core::v1::Namespace;
use kube::{
    Api,
    api::{ListParams, ObjectList},
};
use mirrord_analytics::NullReporter;
use mirrord_config::{
    LayerConfig,
    config::ConfigContext,
    target::{Target, TargetType},
};
use mirrord_kube::{api::kubernetes::seeker::KubeResourceSeeker, error::KubeApiError};
use mirrord_operator::{
    client::{OperatorAccess, OperatorApi},
    crd::{
        MirrordOperatorSpec, NewOperatorFeature, TargetCrd,
        namespaces::{OperatorNamespaceList, namespaces_api_path},
    },
};
use semver::VersionReq;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use tracing::Level;

use crate::{
    CliError, CliResult, Format, ListTargetArgs, kube::kube_client_and_access_from_layer_config,
    util,
};

/// Name of the environment variable used to specify which resource types to list with `mirrord ls`.
/// Primarily used by the plugins when the user picks a target to fetch fewer targets at once.
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
    /// Taken from [`LayerConfig::target`], defaults to [`kube::Client`]'s default namespace.
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
    #[tracing::instrument(level = Level::DEBUG, skip(layer_config), name = "resolve_targets", err)]
    async fn resolve(
        layer_config: LayerConfig,
        rich_output: bool,
        target_types: Option<Vec<TargetType>>,
    ) -> CliResult<Self> {
        let (client, access) = kube_client_and_access_from_layer_config(&layer_config).await?;

        let start = Instant::now();
        let mut reporter = NullReporter::default();
        let progress = mirrord_progress::NullProgress {};
        let operator_api = if layer_config.operator != Some(false)
            && let Some(api) = OperatorApi::try_new(&layer_config, &mut reporter, &progress).await?
        {
            tracing::debug!(elapsed_s = start.elapsed().as_secs_f32(), "Operator found");

            let api = api
                .with_client_certificate(&mut reporter, &progress, &layer_config)
                .await;

            api.inspect_cert_error(
                |error| tracing::error!(%error, "failed to prepare client certificate"),
            );

            Some(api)
        } else {
            None
        };

        let operator_spec = operator_api.as_ref().map(|api| &api.operator().spec);
        // The same namespace a session with this config runs in: the client's default under a
        // login too, since that is what the connect request carries.
        let current_namespace = layer_config
            .target
            .namespace
            .as_deref()
            .unwrap_or(client.default_namespace())
            .to_owned();
        let namespace_source = NamespaceSource::select(rich_output, &access, operator_spec);

        let seeker = KubeResourceSeeker {
            client: &client,
            namespace: &current_namespace,
            copy_target: layer_config.feature.copy_target.enabled,
        };

        let (targets, namespaces) = match &access {
            // Only the operator's own API answers a logged-in CLI, so it lists what the operator
            // sees instead of reading the cluster's resources itself.
            OperatorAccess::Login(_) => {
                targets_through_operator(
                    &client,
                    seeker.namespace,
                    rich_output,
                    target_types.as_deref(),
                    namespace_source,
                )
                .await?
            }
            OperatorAccess::Kubeconfig { .. } => tokio::try_join!(
                async {
                    let paths = match (operator_api, target_types) {
                        (None, _) if layer_config.operator == Some(true) => {
                            Err(CliError::OperatorNotInstalled)
                        }

                        (Some(api), None)
                            if !rich_output
                                && ALL_TARGETS_SUPPORTED_OPERATOR_VERSION
                                    .matches(&api.operator().spec.operator_version) =>
                        {
                            seeker.all().await.map_err(|error| {
                                CliError::friendlier_error_or_else(
                                    error,
                                    CliError::ListTargetsFailed,
                                )
                            })
                        }

                        (Some(api), Some(target_types))
                            if !rich_output
                                && ALL_TARGETS_SUPPORTED_OPERATOR_VERSION
                                    .matches(&api.operator().spec.operator_version) =>
                        {
                            seeker.filtered(target_types, true).await.map_err(|error| {
                                CliError::friendlier_error_or_else(
                                    error,
                                    CliError::ListTargetsFailed,
                                )
                            })
                        }

                        (None, Some(target_types)) => {
                            seeker.filtered(target_types, false).await.map_err(|error| {
                                CliError::friendlier_error_or_else(
                                    error,
                                    CliError::ListTargetsFailed,
                                )
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
                    let namespaces = if namespace_source.is_some() {
                        seeker
                            .list_all_clusterwide::<Namespace>(None)
                            .try_filter_map(|namespace| {
                                std::future::ready(Ok(namespace.metadata.name))
                            })
                            .try_collect::<Vec<_>>()
                            .await
                            .map_err(KubeApiError::KubeError)
                            .map_err(|error| {
                                CliError::friendlier_error_or_else(
                                    error,
                                    CliError::ListTargetsFailed,
                                )
                            })?
                    } else {
                        Default::default()
                    };

                    Ok::<_, CliError>(namespaces)
                }
            )?,
        };

        Ok(Self {
            targets,
            current_namespace,
            namespaces,
        })
    }
}

/// Where the rich `mirrord ls` output, the IDE's picker, takes the namespaces it offers from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamespaceSource {
    /// The cluster's `Namespace` resources, read through the kubeconfig.
    ClusterNamespaces,
    /// The operator's namespace route: the namespaces this user may list targets in, filtered by
    /// the operator with the cluster's RBAC. Only the operator's API answers a logged-in CLI.
    OperatorRoute,
    /// The namespaces of the targets a logged-in CLI lists cluster-wide, for an operator without
    /// the namespace route. RBAC allows that list only for users who may list targets cluster-wide,
    /// so a user bound in some namespaces gets the current namespace alone.
    TargetsAcrossNamespaces,
}

impl NamespaceSource {
    /// `None` for the plain listing, which prints target paths only and reads no namespaces.
    fn select(
        rich_output: bool,
        access: &OperatorAccess,
        operator: Option<&MirrordOperatorSpec>,
    ) -> Option<Self> {
        if rich_output.not() {
            return None;
        }
        let source = match access {
            OperatorAccess::Kubeconfig { .. } => Self::ClusterNamespaces,
            OperatorAccess::Login(_)
                if operator.is_some_and(|spec| {
                    spec.require_feature(NewOperatorFeature::NamespaceRoutes)
                        .is_ok()
                }) =>
            {
                Self::OperatorRoute
            }
            OperatorAccess::Login(_) => Self::TargetsAcrossNamespaces,
        };
        Some(source)
    }
}

/// Target discovery through the operator's `targets` resource, for a CLI logged in through SSO.
///
/// The operator lists every workload the seeker would list from the cluster, already filtered to
/// what the user may target. The rich listing also offers the namespaces the user can switch to,
/// from the [`NamespaceSource`] the operator supports; when that fails, the current namespace is
/// the only one offered and the log says why, so the picker keeps working on the targets it has.
async fn targets_through_operator(
    client: &kube::Client,
    namespace: &str,
    rich_output: bool,
    target_types: Option<&[TargetType]>,
    namespace_source: Option<NamespaceSource>,
) -> CliResult<(Vec<FoundTarget>, Vec<String>)> {
    let list_failed = |error: kube::Error| {
        CliError::friendlier_error_or_else(KubeApiError::from(error), CliError::ListTargetsFailed)
    };

    let listed = Api::<TargetCrd>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await;
    let targets = tolerate_forbidden_namespace(listed, rich_output, namespace)
        .map_err(list_failed)?
        .into_iter()
        .filter_map(|crd| crd.spec.target.as_known().ok().cloned())
        .filter(|target| {
            let wanted_type = match target_types {
                Some(types) => types.contains(&TargetType::from(target)),
                None => true,
            };
            // The rich listing is the IDE's picker, which offers these three kinds only.
            let rich_kind = matches!(
                target,
                Target::Deployment(_) | Target::Rollout(_) | Target::Pod(_)
            );
            wanted_type && (rich_output.not() || rich_kind)
        })
        .map(|target| FoundTarget {
            path: target.to_string(),
            available: true,
        })
        .collect();

    let namespaces = match namespace_source {
        None => Vec::new(),
        Some(NamespaceSource::OperatorRoute) => {
            namespaces_through_operator_route(client, namespace).await
        }
        Some(NamespaceSource::TargetsAcrossNamespaces) => {
            namespaces_of_targets_across_cluster(client, namespace).await
        }
        // The kubeconfig path lists the cluster's namespaces itself and never comes here.
        Some(NamespaceSource::ClusterNamespaces) => vec![namespace.to_owned()],
    };

    Ok((targets, namespaces))
}

/// In the rich listing, a `403` on the current namespace yields no targets instead of failing the
/// command: the IDE's picker still needs the namespaces the user may switch to, and a user bound in
/// some namespaces only often may not list the one the listing starts in. The plain listing keeps
/// the error, since its only output is the targets.
fn tolerate_forbidden_namespace(
    listed: kube::Result<ObjectList<TargetCrd>>,
    rich_output: bool,
    namespace: &str,
) -> kube::Result<Vec<TargetCrd>> {
    match listed {
        Ok(list) => Ok(list.items),
        Err(kube::Error::Api(status)) if rich_output && status.code == 403 => {
            tracing::warn!(
                namespace,
                message = %status.message,
                "Listing targets in the current namespace is forbidden, offering the other namespaces"
            );
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

/// The namespaces the operator says this user may list targets in. On failure, the current
/// namespace alone: a `403` means the user's role lacks `list` on `namespaces` in the operator's
/// API group, which the chart's user roles grant from the version that serves the route.
async fn namespaces_through_operator_route(client: &kube::Client, namespace: &str) -> Vec<String> {
    let request = http::Request::get(namespaces_api_path()).body(Vec::new());
    let listed = match request {
        Ok(request) => client
            .request::<OperatorNamespaceList>(request)
            .await
            .map_err(KubeApiError::from),
        Err(error) => Err(KubeApiError::KubeError(kube::Error::HttpError(error))),
    };
    match listed {
        Ok(list) => {
            let mut namespaces = list.names();
            if namespaces.is_empty() {
                namespaces.push(namespace.to_owned());
            }
            namespaces
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "Listing the namespaces through the operator failed, offering only the current one"
            );
            vec![namespace.to_owned()]
        }
    }
}

/// The namespaces of every target the operator lists cluster-wide, for an operator without the
/// namespace route, with the current namespace always among them so the picker is never left
/// without a choice. On failure, the current namespace alone.
async fn namespaces_of_targets_across_cluster(
    client: &kube::Client,
    namespace: &str,
) -> Vec<String> {
    match Api::<TargetCrd>::all(client.clone())
        .list(&ListParams::default())
        .await
    {
        Ok(all_targets) => {
            let mut namespaces = all_targets
                .into_iter()
                .filter_map(|crd| crd.metadata.namespace)
                .chain(std::iter::once(namespace.to_owned()))
                .collect::<Vec<_>>();
            namespaces.sort();
            namespaces.dedup();
            namespaces
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "Listing targets across namespaces failed, offering only the current one"
            );
            vec![namespace.to_owned()]
        }
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
#[mirrord_macros::boxed_future]
pub(super) async fn print_targets(args: ListTargetArgs, rich_output: bool) -> CliResult<()> {
    let mut cfg_config =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file);

    let mut layer_config = crate::util::resolve_layer_config(&mut cfg_config).await?;

    if let Some(namespace) = args.namespace {
        layer_config.target.namespace.replace(namespace);
    };

    if !layer_config.use_proxy {
        util::remove_proxy_env();
    }

    let target_types = if let Some(target_type) = args.target_type {
        Some(target_type)
    } else {
        match std::env::var(LS_TARGET_TYPES_ENV)
            .ok()
            .map(|val| serde_json::from_str::<Vec<TargetType>>(val.as_ref()))
            .transpose()?
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, Utc};
    use mirrord_operator::sso::{OperatorLogin, OperatorLoginToken};

    use super::*;

    fn login() -> OperatorAccess {
        OperatorAccess::Login(OperatorLogin {
            url: "https://mirrord.corp.example.com".to_owned(),
            token: OperatorLoginToken::from("secret-token".to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at: Utc::now() + TimeDelta::hours(1),
        })
    }

    fn kubeconfig() -> OperatorAccess {
        OperatorAccess::Kubeconfig {
            context: Some("staging".to_owned()),
        }
    }

    /// An operator advertising `features`, with `payments` as its default namespace.
    fn operator(features: &[NewOperatorFeature]) -> MirrordOperatorSpec {
        serde_json::from_value(serde_json::json!({
            "operator_version": "3.236.0",
            "default_namespace": "payments",
            "supported_features": features,
            "license": {"name": "test", "organization": "test", "expire_at": "2099-01-01"}
        }))
        .unwrap()
    }

    /// The plain listing prints target paths only, so no path reads namespaces.
    #[test]
    fn plain_output_reads_no_namespaces() {
        let operator = operator(&[NewOperatorFeature::NamespaceRoutes]);
        assert_eq!(
            NamespaceSource::select(false, &login(), Some(&operator)),
            None
        );
        assert_eq!(NamespaceSource::select(false, &kubeconfig(), None), None);
    }

    /// With a kubeconfig the CLI reads the cluster's namespaces itself, whatever the operator.
    #[test]
    fn kubeconfig_reads_the_clusters_namespaces() {
        let operator = operator(&[NewOperatorFeature::NamespaceRoutes]);
        assert_eq!(
            NamespaceSource::select(true, &kubeconfig(), Some(&operator)),
            Some(NamespaceSource::ClusterNamespaces)
        );
        assert_eq!(
            NamespaceSource::select(true, &kubeconfig(), None),
            Some(NamespaceSource::ClusterNamespaces)
        );
    }

    /// A logged-in CLI asks the operator for the namespaces when the operator serves the route.
    #[test]
    fn login_asks_an_operator_with_the_route() {
        let operator = operator(&[NewOperatorFeature::NamespaceRoutes]);
        assert_eq!(
            NamespaceSource::select(true, &login(), Some(&operator)),
            Some(NamespaceSource::OperatorRoute)
        );
    }

    /// An operator that predates the route still answers a cluster-wide target list, which is
    /// the only namespace source left to a logged-in CLI.
    #[test]
    fn login_falls_back_to_targets_on_an_older_operator() {
        let operator = operator(&[NewOperatorFeature::ProfileRoutes]);
        assert_eq!(
            NamespaceSource::select(true, &login(), Some(&operator)),
            Some(NamespaceSource::TargetsAcrossNamespaces)
        );
        assert_eq!(
            NamespaceSource::select(true, &login(), None),
            Some(NamespaceSource::TargetsAcrossNamespaces)
        );
    }

    fn forbidden(code: u16) -> kube::Result<ObjectList<TargetCrd>> {
        Err(kube::Error::Api(Box::new(kube::core::Status {
            status: Some(kube::core::response::StatusSummary::Failure),
            message: "targets.operator.metalbear.co is forbidden".to_owned(),
            reason: "Forbidden".to_owned(),
            code,
            ..Default::default()
        })))
    }

    /// A user bound in `team-a` only starts the IDE picker in the client's default namespace,
    /// which refuses the target list with `403`. The rich listing goes on with no targets, so the
    /// namespaces still reach the picker; the plain listing, and any other error, still fail.
    #[test]
    fn a_forbidden_current_namespace_empties_the_rich_listing_only() {
        let rich = tolerate_forbidden_namespace(forbidden(403), true, "default").unwrap();
        assert!(rich.is_empty());

        assert!(tolerate_forbidden_namespace(forbidden(403), false, "default").is_err());
        assert!(tolerate_forbidden_namespace(forbidden(500), true, "default").is_err());
    }
}
