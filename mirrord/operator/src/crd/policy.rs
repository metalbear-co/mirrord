use std::collections::HashSet;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::label_selector::LabelSelector;

/// Features and operations that can be blocked by `mirrordpolicies` and `mirrordclusterpolicies`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")] // StealWithoutFilter -> steal-without-filter in yaml.
pub enum BlockedFeature {
    /// Blocks stealing traffic in any way (without or without filter).
    Steal,

    /// Blocks stealing traffic without specifying (any) filter. Client can still specify a
    /// filter that matches anything.
    StealWithoutFilter,

    /// Blocks mirroring traffic.
    Mirror,

    /// So that the operator is able to list all policies with [`kube::Api`],
    /// even if it doesn't recognize blocked features used in some of them.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Custom resource for policies that limit what mirrord features users can use.
///
/// This policy applies only to resources living in the same namespace.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want policies to be handled by k8s.
    group = "policies.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordPolicy",
    namespaced
)]
#[serde(rename_all = "camelCase")] // target_path -> targetPath in yaml.
pub struct MirrordPolicySpec {
    /// Specify the targets for which this policy applies, in the pod/my-pod deploy/my-deploy
    /// notation. Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this policy does not depend on the target's path.
    pub target_path: Option<String>,

    /// If specified in a policy, the policy will only apply to targets with labels that match all
    /// of the selector's rules.
    pub selector: Option<LabelSelector>,

    // TODO: make the k8s list type be set/map to prevent duplicates.
    /// List of features and operations blocked by this policy.
    pub block: Vec<BlockedFeature>,

    /// Controls how mirrord-operator handles user requests to fetch environment variables from the
    /// target.
    #[serde(default)]
    pub env: EnvPolicy,

    /// Overrides fs ops behaviour, granting control over them to the operator policy, instead of
    /// the user config.
    #[serde(default)]
    pub fs: FsPolicy,
}

/// Custom cluster-wide resource for policies that limit what mirrord features users can use.
///
/// This policy applies to resources across all namespaces in the cluster.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want policies to be handled by k8s.
    group = "policies.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterPolicy"
)]
#[serde(rename_all = "camelCase")] // target_path -> targetPath in yaml.
pub struct MirrordClusterPolicySpec {
    /// Specify the targets for which this policy applies, in the pod/my-pod deploy/my-deploy
    /// notation. Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this policy does not depend on the target's path.
    pub target_path: Option<String>,

    /// If specified in a policy, the policy will only apply to targets with labels that match all
    /// of the selector's rules.
    pub selector: Option<LabelSelector>,

    // TODO: make the k8s list type be set/map to prevent duplicates.
    /// List of features and operations blocked by this policy.
    pub block: Vec<BlockedFeature>,

    /// Controls how mirrord-operator handles user requests to fetch environment variables from the
    /// target.
    #[serde(default)]
    pub env: EnvPolicy,

    /// Overrides fs ops behaviour, granting control over them to the operator policy, instead of
    /// the user config.
    #[serde(default)]
    pub fs: FsPolicy,

    #[serde(default)]
    pub network: NetworkPolicy,
}

/// Policy for controlling environment variables access from mirrord instances.
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvPolicy {
    /// List of environment variables that should be excluded when using mirrord.
    ///
    /// These environment variables won't be retrieved from the target even if the user
    /// specifies them in their `feature.env.include` mirrord config.
    ///
    /// Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
    /// any character and `*` matches arbitrary many (including zero) occurrences of any character,
    /// e.g. `DATABASE_*` will match `DATABASE_URL` and `DATABASE_PORT`.
    #[serde(default)]
    pub exclude: HashSet<String>,
}

/// File operations policy that mimics the mirrord fs config.
///
/// Allows the operator control over remote file ops behaviour, overriding what the user has set in
/// their mirrord config file, if it matches something in one of the lists (regex sets) of this
/// struct.
///
/// If the file path matches regexes in multiple sets, priority is as follows:
/// 1. `local`
/// 2. `notFound`
/// 3. `readOnly`
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FsPolicy {
    /// Files that cannot be opened for writing.
    ///
    /// Opening the file for writing is rejected with an IO error.
    #[serde(default)]
    pub read_only: HashSet<String>,

    /// Files that cannot be opened at all.
    ///
    /// Opening the file will be rejected and mirrord will open the file locally instead.
    #[serde(default)]
    pub local: HashSet<String>,

    /// Files that cannot be opened at all.
    ///
    /// Opening the file is rejected with an IO error.
    #[serde(default)]
    pub not_found: HashSet<String>,
}

#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicy {
    #[serde(default)]
    pub incoming: IncomingNetworkPolicy,
}

#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IncomingNetworkPolicy {
    #[serde(default)]
    pub filter: HttpFilterPolicy,
}

#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HttpFilterPolicy {
    pub header_filter: Option<String>,
}

#[test]
fn check_one_api_group() {
    use kube::Resource;

    assert_eq!(MirrordPolicy::group(&()), MirrordClusterPolicy::group(&()),)
}
