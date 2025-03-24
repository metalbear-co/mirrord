//! This module contains utilities for working with [`MirrordProfile`]s.

use kube::{Api, Client};
use miette::Diagnostic;
use mirrord_config::{
    feature::{network::incoming::IncomingMode, FeatureConfig},
    util::VecOrSingle,
    LayerConfig,
};
use mirrord_kube::{api::kubernetes::create_kube_config, error::KubeApiError};
use mirrord_operator::crd::profile::{
    FeatureAdjustment, FeatureChange, MirrordProfile, MirrordProfileSpec,
};
use mirrord_progress::Progress;
use thiserror::Error;

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
        "Use `kubectl get mirrordprofiles <profile-name>` \
        to verify that the profile exists \
        and its name is spelled correctly in your mirrord config."
    ))]
    ProfileNotFound,

    #[error("failed to fetch the mirrord profile: {0}")]
    #[diagnostic(help(
        "Check if you have sufficient permissions to fetch the profile from the cluster."
    ))]
    ProfileFetchError(KubeApiError),
}

/// Convenience trait for [`FeatureAdjustment`].
trait FeatureAdjustmentExt: Sized {
    /// Tries to apply this adjustment to the given [`FeatureConfig`].
    fn apply_to(self, config: &mut FeatureConfig) -> Result<(), ProfileError>;
}

impl FeatureAdjustmentExt for FeatureAdjustment {
    fn apply_to(self, config: &mut FeatureConfig) -> Result<(), ProfileError> {
        let Self {
            change,
            unknown_fields,
        } = self;

        if let Some(field) = unknown_fields.into_keys().next() {
            return Err(ProfileError::UnknownField(field));
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

/// Applies the given [`MirrordProfile`] to the given [`FeatureConfig`].
fn apply_profile(config: &mut FeatureConfig, profile: MirrordProfile) -> Result<(), ProfileError> {
    let MirrordProfileSpec {
        feature_adjustments,
        unknown_fields,
    } = profile.spec;

    if let Some(field) = unknown_fields.into_keys().next() {
        return Err(ProfileError::UnknownField(field));
    }

    for adjustment in feature_adjustments {
        adjustment.apply_to(config)?;
    }

    Ok(())
}

/// If the [`LayerConfig::profile`] field specifies a [`MirrordProfile`] to use,
/// fetches that profile from the cluster and applies it to the config.
///
/// Verifies that the fetched profile does not contain any unknown fields or values.
pub async fn apply_profile_if_configured<P: Progress>(
    config: &mut LayerConfig,
    progress: &P,
) -> Result<(), CliError> {
    let Some(name) = config.profile.as_deref() else {
        return Ok(());
    };

    let mut subtask = progress.subtask(&format!("fetching mirrord profile `{name}`"));
    let profile: Result<MirrordProfile, KubeApiError> = try {
        let client: Client = create_kube_config(
            config.accept_invalid_certificates,
            config.kubeconfig.as_deref(),
            config.kube_context.clone(),
        )
        .await?
        .try_into()
        .map_err(KubeApiError::from)?;

        let api = Api::<MirrordProfile>::all(client);
        api.get(name).await?
    };

    let profile = match profile {
        Ok(profile) => profile,
        Err(KubeApiError::KubeError(kube::Error::Api(error))) if error.code == 404 => {
            return Err(CliError::ProfileError(ProfileError::ProfileNotFound));
        }
        Err(error) => {
            return Err(CliError::friendlier_error_or_else(error, |error| {
                CliError::ProfileError(ProfileError::ProfileFetchError(error))
            }));
        }
    };
    subtask.success(Some(&format!("mirrord profile `{name}` fetched")));

    let mut subtask = progress.subtask(&format!("applying mirrord profile `{name}`"));
    apply_profile(&mut config.feature, profile)?;
    subtask.success(Some(&format!("mirrord profile `{name}` applied")));

    Ok(())
}
