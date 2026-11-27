//! This module contains utilities for working with [`MirrordClusterProfile`]s and
//! [`MirrordProfile`]s.

use std::{fmt::Display, str::FromStr};

use http::StatusCode;
use kube::{Api, client::ClientBuilder};
use miette::Diagnostic;
use mirrord_config::{
    LayerConfig,
    feature::{FeatureConfig, network::incoming::IncomingMode},
    util::VecOrSingle,
};
use mirrord_kube::{error::KubeApiError, retry::retry_policy_from_config};
use mirrord_operator::{
    client::{NoClientCert, OperatorAccess, OperatorApi},
    crd::{
        MirrordOperatorCrd, NewOperatorFeature, OPERATOR_STATUS_NAME,
        profile::{
            FeatureAdjustment, FeatureChange, MirrordClusterProfile, MirrordProfile,
            cluster_profile_api_path, namespaced_profile_api_path,
        },
    },
    types::PaidFeatureGate,
};
use mirrord_progress::Progress;
use thiserror::Error;
use tower::{buffer::BufferLayer, retry::RetryLayer};
use tracing::Level;

use crate::CliError;

/// Errors that can occur when fetching or applying a [`MirrordProfile`].
#[derive(Error, Debug, Diagnostic)]
pub enum ProfileError {
    #[error("mirrord profile contains an unknown value")]
    #[diagnostic(help("Consider upgrading your mirrord binary."))]
    UnknownVariant,

    #[error("mirrord profile contains an unknown field `{0}`")]
    #[diagnostic(help("Consider upgrading your mirrord binary."))]
    UnknownField(String),

    #[error("mirrord profile was not found in the cluster")]
    #[diagnostic(help(
        "Verify that the profile exists \
        and its name is spelled correctly in your mirrord config. \
        For cluster-wide profile, use `kubectl get mirrordclusterprofiles <profile-name>`. \
        For namespaced profile, use `kubectl get mirrordprofiles <profile-name> -n <namespace>`."
    ))]
    ProfileNotFound,

    #[error("failed to fetch the mirrord profile: {0}")]
    #[diagnostic(help(
        "Check if you have sufficient permissions to fetch the profile from the cluster."
    ))]
    ProfileFetchError(KubeApiError),

    #[error("mirrord profile must be cluster-wide or in the same namespace as the target ({0})")]
    NamespaceConflict(String),

    /// The profile's name does not form a request path, so the operator was never asked.
    #[error("failed to build the request for the mirrord profile: {0}")]
    ProfileRequest(#[source] http::Error),
}

/// Identifier of [`MirrordClusterProfile`] and [`MirrordProfile`].
#[derive(Debug, Eq, PartialEq)]
enum ProfileIdentifier {
    Namespaced { namespace: String, profile: String },
    Cluster(String),
}

impl Display for ProfileIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileIdentifier::Cluster(profile) => write!(f, "{profile}"),
            ProfileIdentifier::Namespaced { namespace, profile } => {
                write!(f, "{namespace}/{profile}")
            }
        }
    }
}

impl FromStr for ProfileIdentifier {
    type Err = ProfileError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((namespace, profile)) => Ok(Self::Namespaced {
                namespace: namespace.to_owned(),
                profile: profile.to_owned(),
            }),
            None => Ok(Self::Cluster(s.to_owned())),
        }
    }
}

enum ProfileFetchResult {
    Cluster(MirrordClusterProfile),
    Namespaced(MirrordProfile),
}

impl ProfileFetchResult {
    fn feature_adjustments(&self) -> impl Iterator<Item = &FeatureAdjustment> {
        match self {
            ProfileFetchResult::Cluster(profile) => profile.spec.feature_adjustments.iter(),
            ProfileFetchResult::Namespaced(profile) => profile.spec.feature_adjustments.iter(),
        }
    }

    fn unknown_fields(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
        match self {
            ProfileFetchResult::Cluster(profile) => profile.spec.unknown_fields.iter(),
            ProfileFetchResult::Namespaced(profile) => profile.spec.unknown_fields.iter(),
        }
    }
}

/// Convenience trait for [`FeatureAdjustment`].
trait FeatureAdjustmentExt: Sized {
    /// Tries to apply this adjustment to the given [`FeatureConfig`].
    fn apply_to(&self, config: &mut FeatureConfig) -> Result<(), ProfileError>;
}

impl FeatureAdjustmentExt for FeatureAdjustment {
    fn apply_to(&self, config: &mut FeatureConfig) -> Result<(), ProfileError> {
        let Self {
            change,
            unknown_fields,
        } = self;

        if let Some(field) = unknown_fields.keys().next() {
            return Err(ProfileError::UnknownField(field.to_string()));
        }

        match change {
            FeatureChange::IncomingOff => {
                config.network.incoming.mode = IncomingMode::Off;
            }
            FeatureChange::IncomingMirror => {
                config.network.incoming.mode = IncomingMode::Mirror;
            }
            FeatureChange::IncomingSteal => config.network.incoming.mode = IncomingMode::Steal,
            FeatureChange::DnsOff => {
                config.network.dns.enabled = false;
                config.network.dns.filter = None;
            }
            FeatureChange::DnsRemote => {
                config.network.dns.enabled = true;
                config.network.dns.filter = None;
            }
            FeatureChange::OutgoingOff => {
                config.network.outgoing.tcp = false;
                config.network.outgoing.udp = false;
                config.network.outgoing.filter = None;
                config.network.outgoing.unix_streams = None;
            }
            FeatureChange::OutgoingRemote => {
                config.network.outgoing.tcp = true;
                config.network.outgoing.udp = true;
                config.network.outgoing.filter = None;
                config.network.outgoing.unix_streams = Some(VecOrSingle::Single(".*".into()));
            }
            FeatureChange::Unknown => return Err(ProfileError::UnknownVariant),
        }

        Ok(())
    }
}

/// Applies the given profile to the given [`LayerConfig`].
fn apply_profile<P: Progress>(
    config: &mut LayerConfig,
    profile: &ProfileFetchResult,
    profile_identifier: &ProfileIdentifier,
    subtask: &mut P,
) -> Result<(), ProfileError> {
    if let Some((field, _)) = profile.unknown_fields().next() {
        return Err(ProfileError::UnknownField(field.to_string()));
    }

    if let ProfileIdentifier::Namespaced {
        namespace: profile_ns,
        profile: _,
    } = profile_identifier
    {
        if let Some(target_ns) = &config.target.namespace {
            if target_ns != profile_ns {
                return Err(ProfileError::NamespaceConflict(target_ns.clone()));
            }
        } else {
            subtask.info(&format!("setting target namespace to {profile_ns}"));
            config.target.namespace = Some(profile_ns.to_string());
            crate::kube::update_run_target_namespace(config);
        }
    }

    for adjustment in profile.feature_adjustments() {
        adjustment.apply_to(&mut config.feature)?;
    }

    Ok(())
}

/// If the [`LayerConfig::profile`] field specifies a [`MirrordClusterProfile`] or
/// [`MirrordProfile`] to use, fetches that profile from the cluster and applies it
/// to the config.
///
/// Verifies that the fetched profile does not contain any unknown fields or values.
pub async fn apply_profile_if_configured<P: Progress>(
    config: &mut LayerConfig,
    progress: &P,
) -> Result<(), CliError> {
    let Some(name) = config.profile.as_deref() else {
        return Ok(());
    };

    let profile_identifier = name.parse()?;
    let mut subtask = progress.subtask(&format!("fetching mirrord profile `{profile_identifier}`"));

    let profile = fetch_profile(config, &profile_identifier).await?;
    subtask.success(Some(&format!(
        "mirrord profile `{profile_identifier}` fetched"
    )));
    let mut subtask = progress.subtask(&format!("applying mirrord profile `{profile_identifier}`"));
    apply_profile(config, &profile, &profile_identifier, &mut subtask)?;
    subtask.success(Some(&format!(
        "mirrord profile `{profile_identifier}` applied"
    )));
    Ok(())
}

/// Reads the profile the way the run reaches the cluster: from the profile CRDs through the
/// kubeconfig, or from the operator's own API under an operator login, where only the operator's
/// API group is reachable.
#[tracing::instrument(level = Level::TRACE, err)]
async fn fetch_profile(
    layer_config: &LayerConfig,
    profile_identifier: &ProfileIdentifier,
) -> Result<ProfileFetchResult, CliError> {
    let (config, access) = OperatorApi::<NoClientCert>::base_client_config(layer_config).await?;

    let client = ClientBuilder::try_from(config)
        .map_err(KubeApiError::from)
        .map_err(fetch_error)?
        .with_layer(&BufferLayer::new(1024))
        .with_layer(&RetryLayer::new(
            retry_policy_from_config(&layer_config.startup_retry)
                .map_err(KubeApiError::from)
                .map_err(fetch_error)?,
        ))
        .build();

    let fetched = match access {
        OperatorAccess::Kubeconfig { .. } => match profile_identifier {
            ProfileIdentifier::Cluster(profile) => Api::<MirrordClusterProfile>::all(client)
                .get(profile)
                .await
                .map(ProfileFetchResult::Cluster),
            ProfileIdentifier::Namespaced { namespace, profile } => {
                Api::<MirrordProfile>::namespaced(client, namespace)
                    .get(profile)
                    .await
                    .map(ProfileFetchResult::Namespaced)
            }
        },
        OperatorAccess::Login(..) => {
            // An operator without the routes answers them with the same 404 a missing profile
            // gets, so the feature is checked first to tell the two apart.
            let operator = Api::<MirrordOperatorCrd>::all(client.clone())
                .get(OPERATOR_STATUS_NAME)
                .await
                .map_err(KubeApiError::from)
                .map_err(fetch_error)?;
            operator
                .spec
                .require_feature(NewOperatorFeature::ProfileRoutes)?;

            let get = |path: String| {
                http::Request::get(path)
                    .body(Vec::new())
                    .map_err(|error| CliError::ProfileError(ProfileError::ProfileRequest(error)))
            };
            match profile_identifier {
                ProfileIdentifier::Cluster(profile) => client
                    .request::<MirrordClusterProfile>(get(cluster_profile_api_path(profile))?)
                    .await
                    .map(ProfileFetchResult::Cluster),
                ProfileIdentifier::Namespaced { namespace, profile } => client
                    .request::<MirrordProfile>(get(namespaced_profile_api_path(
                        namespace, profile,
                    ))?)
                    .await
                    .map(ProfileFetchResult::Namespaced),
            }
        }
    };

    fetched.map_err(|error| match error {
        // The operator refuses the route below Enterprise with the paid-feature status every
        // other gated feature gives, which names the plan instead of a bare 403.
        kube::Error::Api(status) if let Some(gate) = PaidFeatureGate::from_status(&status) => {
            CliError::PaidFeatureRequiresPlan { gate }
        }
        kube::Error::Api(status) if status.code == StatusCode::NOT_FOUND => {
            CliError::ProfileError(ProfileError::ProfileNotFound)
        }
        error => fetch_error(KubeApiError::from(error)),
    })
}

/// A profile fetch that failed for a reason other than a missing profile, with the friendlier
/// cluster-access diagnosis when there is one.
fn fetch_error(error: KubeApiError) -> CliError {
    CliError::friendlier_error_or_else(error, |error| {
        CliError::ProfileError(ProfileError::ProfileFetchError(error))
    })
}

#[cfg(test)]
mod test {
    use crate::profile::ProfileIdentifier;

    #[test]
    fn test_profile_name() {
        assert_eq!(
            "my-profile".parse::<ProfileIdentifier>().unwrap(),
            ProfileIdentifier::Cluster("my-profile".to_owned())
        );
        assert_eq!(
            "my-namespace/my-profile"
                .parse::<ProfileIdentifier>()
                .unwrap(),
            ProfileIdentifier::Namespaced {
                namespace: "my-namespace".to_owned(),
                profile: "my-profile".to_owned()
            }
        );
        assert_eq!(
            "a/b/c".parse::<ProfileIdentifier>().unwrap(),
            ProfileIdentifier::Namespaced {
                namespace: "a".to_owned(),
                profile: "b/c".to_owned()
            }
        );

        assert_eq!(
            "/my-profile".parse::<ProfileIdentifier>().unwrap(),
            ProfileIdentifier::Namespaced {
                namespace: "".to_owned(),
                profile: "my-profile".to_owned()
            }
        );
    }
}
