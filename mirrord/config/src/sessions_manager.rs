use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Selects the sessions-manager that pairs a serverless target with the agent running beside it.
///
/// Without it, mirrord connects through the MetalBear Cloud sessions-manager, unless
/// [`operator`](#root-operator) is `true`, in which case the sessions-manager hosted by the
/// mirrord operator is used and this section is ignored.
///
/// ```json
/// {
///   "sessions_manager": {
///     "url": "https://sessions-manager.example.com/sm"
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[config(map_to = "SessionsManagerFileConfig", derive = "JsonSchema, Serialize")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct SessionsManagerConfig {
    /// ### sessions_manager.url {#sessions_manager-url}
    ///
    /// Base URL of a standalone sessions-manager, including any path prefix it is served under,
    /// e.g. `https://example.com/sm`.
    ///
    /// The `MIRRORD_SESSIONS_MANAGER_URL` environment variable overrides it.
    ///
    /// Defaults to the MetalBear Cloud sessions-manager,
    /// `https://sm-aws-use1.app.metalbear.com/sm`.
    #[config(env = "MIRRORD_SESSIONS_MANAGER_URL")]
    pub url: Option<String>,
}
