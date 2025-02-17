use kube::CustomResource;
use mirrord_agent_env::steal_tls::StealPortTlsConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::label_selector::LabelSelector;

/// Custom resource for configuring how the mirrord-agent handles stealing TLS traffic from selected
/// targets.
///
/// Namespaced.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordTlsStealConfig",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordTlsStealConfigSpec {
    /// Specify the targets for which this configuration applies, in the `pod/my-pod`,
    /// `deploy/my-deploy/container/my-container` notation.
    ///
    /// Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this configuration does not depend on the target's
    /// path.
    pub target_path: Option<String>,
    /// If this selector is provided, this configuration will only apply to targets with labels
    /// that match all of the selector's rules.
    pub selector: Option<LabelSelector>,
    /// Configuration for stealing TLS traffic, separate for each port.
    pub ports: Vec<StealPortTlsConfig>,
}

/// Custom resource for configuring how the mirrord-agent handles stealing TLS traffic from selected
/// targets.
///
/// Clusterwide.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterTlsStealConfig"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterTlsStealConfigSpec {
    /// Specify the targets for which this configuration applies, in the `pod/my-pod`,
    /// `deploy/my-deploy/container/my-container` notation.
    ///
    /// Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this configuration does not depend on the target's
    /// path.
    pub target_path: Option<String>,
    /// If this selector is provided, this configuration will only apply to targets with labels
    /// that match all of the selector's rules.
    pub selector: Option<LabelSelector>,
    /// Configuration for stealing TLS traffic, separate for each port.
    pub ports: Vec<StealPortTlsConfig>,
}

#[test]
fn check_one_api_group() {
    use kube::Resource;

    assert_eq!(
        MirrordTlsStealConfig::group(&()),
        MirrordClusterTlsStealConfig::group(&()),
    )
}
