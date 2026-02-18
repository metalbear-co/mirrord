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
    /// ### feature.magic.aws {#feature-magic-aws}
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
}

impl CollectAnalytics for &MagicConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("aws", self.aws);
    }
}
