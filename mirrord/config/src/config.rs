pub mod context;
pub mod deprecated;
pub mod from_env;
pub mod source;
pub mod unstable;

use std::{error::Error, fmt, io, path::PathBuf};

pub use context::ConfigContext;
use thiserror::Error;

pub use crate::env_key::EnvKey;
use crate::feature::split_queues::QueueSplittingVerificationError;

/// <!--${internal}-->
/// Error that would be returned from [MirrordConfig::generate_config]
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid target provided `{0}`!")]
    InvalidTarget(String),

    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),

    #[error("invalid {} value `{}`: {}", .name, .provided, .error)]
    InvalidValue {
        // Name of parsed env var or field path in the config.
        name: &'static str,
        // Value provided by the user.
        provided: String,
        // Error that occurred when processing the value.
        error: Box<dyn Error + Send + Sync>,
    },

    #[error("failed to read config from file: {0}")]
    FromFileError(#[from] FromFileError),

    #[error("Invalid FS mode `{0}`!")]
    InvalidFsMode(String),

    #[error("Conflicting configuration found `{0}`")]
    Conflict(String),

    #[error(
        "A target namespace was specified, but no target was specified. If you want to set the \
        namespace in which the agent will be created, please set the agent namespace, not the \
        target namespace. That value can be set with agent.namespace in the configuration file, \
        the -a argument of the CLI, or the MIRRORD_AGENT_NAMESPACE environment variable.

        If you are not trying to run targetless, please specify a target instead."
    )]
    TargetNamespaceWithoutTarget,

    #[error(
        "A Job or CronJob target has been specified, but the feature `copy_target` has not been enabled!

        If you want to target a job or cronjob, please enable `copy_target` feature in the `feature` section.
        "
    )]
    TargetJobWithoutCopyTarget,

    #[error(
        "Target type requires the mirrord-operator, but operator usage was explicitly disabled. Consider enabling mirrord-operator in your mirrord config."
    )]
    TargetRequiresOperator,

    #[error("Queue splitting config is invalid: {0}")]
    QueueSplittingVerificationError(#[from] QueueSplittingVerificationError),

    /// When preparing the `EnvVarsRemapper`, regex creation may fail.
    #[error(
        "Regex creation for pattern `{pattern}: {value}` in `config.feature.env.mapping` failed with: `{fail}`"
    )]
    Regex {
        pattern: String,
        value: String,
        fail: Box<fancy_regex::Error>,
    },

    #[error("Decoding resolved config failed: {0}")]
    DecodeError(String),

    #[error("Encoding resolved config failed: {0}")]
    EncodeError(String),

    #[error("Failed to access file {}: {}", path.display(), error)]
    FileAccessFailed { path: PathBuf, error: io::Error },
}

/// Errors that can occur when parsing configuration from a file.
#[derive(Error, Debug)]
pub enum FromFileError {
    Read(#[from] io::Error),
    InvalidExtension(Option<String>),
    TeraRender(#[source] Box<dyn std::error::Error + Send + Sync>),
    ParseToml(#[from] toml::de::Error),
    ParseJson(#[from] serde_json::Error),
    ParseYaml(#[from] serde_yaml::Error),
}

impl From<tera::Error> for FromFileError {
    fn from(error: tera::Error) -> Self {
        Self::TeraRender(error.into())
    }
}

impl fmt::Display for FromFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let error: &dyn std::error::Error = match self {
            Self::InvalidExtension(None) => {
                return f.write_str(
                    "the file has no extension, must have one of: \
                    json, toml, yml, yaml",
                );
            }
            Self::InvalidExtension(Some(ext)) => {
                return write!(
                    f,
                    "the file has an invalid extension `{ext}`, \
                    must have one of: \
                    json, toml, yml, yaml",
                );
            }
            Self::TeraRender(error) => {
                f.write_str("failed to render Tera")?;
                error.as_ref()
            }
            Self::ParseToml(error) => {
                f.write_str("failed to parse Toml")?;
                error
            }
            Self::ParseJson(error) => {
                f.write_str("failed to parse Json")?;
                error
            }
            Self::ParseYaml(error) => {
                f.write_str("failed to parse Yaml")?;
                error
            }
            Self::Read(error) => {
                f.write_str("failed to read the file")?;
                error
            }
        };

        let mut source = error.source();
        while let Some(error) = source {
            write!(f, " -> {error}")?;
            source = error.source();
        }

        Ok(())
    }
}

pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

/// <!--${internal}-->
/// Main configuration creation trait of mirrord-config
pub trait MirrordConfig {
    /// <!--${internal}-->
    /// The resulting struct you plan on using in the rest of your code
    type Generated;

    /// <!--${internal}-->
    /// Load configuration from all sources and output as `Self::Generated`
    /// Pass reference to list of warnings which callee can add warnings into.
    fn generate_config(self, context: &mut ConfigContext) -> Result<Self::Generated>;
}

impl<T> MirrordConfig for Option<T>
where
    T: MirrordConfig + Default,
{
    type Generated = T::Generated;

    fn generate_config(self, context: &mut ConfigContext) -> Result<Self::Generated> {
        self.unwrap_or_default().generate_config(context)
    }
}

/// <!--${internal}-->
/// Lookup trait for accessing type implementing [MirrordConfig] from [MirrordConfig::Generated]
pub trait FromMirrordConfig {
    type Generator: MirrordConfig;
}
