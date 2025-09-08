//! This module contains utilities for working with [`MirrordClusterProfile`]s and
//! [`MirrordProfile`]s.

use std::{fmt::Display, str::FromStr};

use kube::{Api, Client};
use miette::Diagnostic;
use mirrord_config::{
    LayerConfig,
    feature::{FeatureConfig, network::incoming::IncomingMode},
    util::VecOrSingle,
};
use mirrord_kube::{api::kubernetes::create_kube_config, error::KubeApiError};
use mirrord_operator::crd::profile::{
    FeatureAdjustment, FeatureChange, MirrordClusterProfile, MirrordProfile,
};
use mirrord_progress::Progress;
use thiserror::Error;
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
                namespace: namespace.to_string(),
                profile: profile.to_string(),
            }),
            None => Ok(Self::Cluster(s.to_string())),
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

    match fetch_profile(config, &profile_identifier).await {
        Ok(profile) => {
            subtask.success(Some(&format!(
                "mirrord profile `{profile_identifier}` fetched"
            )));
            let mut subtask =
                progress.subtask(&format!("applying mirrord profile `{profile_identifier}`"));
            apply_profile(config, &profile, &profile_identifier, &mut subtask)?;
            subtask.success(Some(&format!(
                "mirrord profile `{profile_identifier}` applied"
            )));
            Ok(())
        }
        Err(KubeApiError::KubeError(kube::Error::Api(error))) if error.code == 404 => {
            Err(CliError::ProfileError(ProfileError::ProfileNotFound))
        }
        Err(error) => Err(CliError::friendlier_error_or_else(error, |error| {
            CliError::ProfileError(ProfileError::ProfileFetchError(error))
        })),
    }
}

#[tracing::instrument(level = Level::INFO, err)]
async fn fetch_profile(
    config: &LayerConfig,
    profile_identifier: &ProfileIdentifier,
) -> Result<ProfileFetchResult, KubeApiError> {
    let client: Client = create_kube_config(
        config.accept_invalid_certificates,
        config.kubeconfig.as_deref(),
        config.kube_context.clone(),
    )
    .await?
    .try_into()
    .map_err(KubeApiError::from)?;

    match profile_identifier {
        ProfileIdentifier::Cluster(profile) => {
            let api = Api::<MirrordClusterProfile>::all(client);
            Ok(ProfileFetchResult::Cluster(api.get(profile).await?))
        }
        ProfileIdentifier::Namespaced { namespace, profile } => {
            let api = Api::<MirrordProfile>::namespaced(client, namespace);
            Ok(ProfileFetchResult::Namespaced(api.get(profile).await?))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::profile::ProfileIdentifier;

    #[test]
    fn test_profile_name() {
        assert_eq!(
            "my-profile".parse::<ProfileIdentifier>().unwrap(),
            ProfileIdentifier::Cluster("my-profile".to_string())
        );
        assert_eq!(
            "my-namespace/my-profile"
                .parse::<ProfileIdentifier>()
                .unwrap(),
            ProfileIdentifier::Namespaced {
                namespace: "my-namespace".to_string(),
                profile: "my-profile".to_string()
            }
        );
        assert_eq!(
            "a/b/c".parse::<ProfileIdentifier>().unwrap(),
            ProfileIdentifier::Namespaced {
                namespace: "a".to_string(),
                profile: "b/c".to_string()
            }
        );

        assert_eq!(
            "/my-profile".parse::<ProfileIdentifier>().unwrap(),
            ProfileIdentifier::Namespaced {
                namespace: "".to_string(),
                profile: "my-profile".to_string()
            }
        );
    }
}
