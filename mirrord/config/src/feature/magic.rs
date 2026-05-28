use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Sensible default behaviors that help most users. Disable individual flags only if they conflict
/// with your setup.
///
/// ```json
/// {
///   "feature": {
///     "magic": {
///       "aws": true
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "MagicFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct MagicConfig {
    /// #### feature.magic.aws {#feature-magic-aws}
    ///
    /// The AWS CLI prefers local credentials (e.g. `~/.aws`, `AWS_PROFILE`) over the remote pod's
    /// identity (IAM role, instance profile, IRSA). When those local credentials are present, the
    /// pod's own identity is never used, which is rarely what you want in a mirrord session.
    ///
    /// When enabled, mirrord makes local AWS configuration unavailable to the process by:
    /// - Unsetting `AWS_PROFILE` and related AWS environment variables.
    /// - Mapping `~/.aws` to a temporary directory, so the AWS CLI cannot read local credentials
    ///   and also has a writable location for its credential cache (avoiding errors on cache
    ///   writes).
    ///
    /// This allows the remote pod's IAM role / instance profile to be used as intended.
    ///
    /// Disable this only if you intentionally need local AWS credentials inside the local mirrord'
    /// process.
    ///
    /// Defaults to `true`.
    #[config(default = true)]
    pub aws: bool,

    /// #### feature.magic.auto_mount {#feature-magic-auto_mount}
    ///
    /// Kubernetes mounts ConfigMaps, Secrets, and volumes (e.g. PVCs) into the target container at
    /// fixed paths. When mirrord runs your process locally, those paths either don't exist or hold
    /// unrelated local data, so the process can't read the configuration or secrets it expects
    /// from the pod.
    ///
    /// When enabled, mirrord reads the target container's volume mounts from the remote pod spec
    /// and adds their mount paths to [`feature.fs.read_only`](#feature-fs-read_only), so the
    /// local process transparently reads those files from the remote pod while still writing
    /// locally.
    ///
    /// This matters when the file system mode is `localwithoverrides`: the mounted paths would
    /// otherwise be served locally and not exist, so `auto_mount` forces them to be read from the
    /// remote pod. In the default `read` mode files are already read remotely, and in fully
    /// `local` mode all overrides are ignored, so `auto_mount` has no observable effect in
    /// either of those modes.
    ///
    /// Paths you've explicitly marked local via `feature.fs.local` are left untouched.
    ///
    /// Disable this only if you intentionally want the mounted paths served from the local file
    /// system.
    ///
    /// Defaults to `true`.
    #[config(default = true)]
    pub auto_mount: bool,
}

impl CollectAnalytics for &MagicConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("aws", self.aws);
        analytics.add("auto_mount", self.auto_mount);
    }
}
