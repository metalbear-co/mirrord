pub mod deprecated;
pub mod from_env;
pub mod source;
pub mod unstable;

use thiserror::Error;

/// <!--${internal}-->
/// Error that would be returned from [MirrordConfig::generate_config]
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid target provided `{0}`!")]
    InvalidTarget(String),

    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),

    #[error("value {0:?} for {1:?} is invalid.")]
    InvalidValue(String, &'static str),

    #[error("mirrord-config: IO operation failed with `{0}`")]
    Io(#[from] std::io::Error),

    #[error("mirrord-config: `{0}`!")]
    SerdeJson(#[from] serde_json::Error),

    #[error("mirrord-config: `{0}`!")]
    Toml(#[from] toml::de::Error),

    #[error("mirrord-config: `{0}`!")]
    SerdeYaml(#[from] serde_yaml::Error),

    #[error("mirrord-config: Unsupported configuration file format!")]
    UnsupportedFormat,

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

    #[error("Template rendering failed, check your config file `{0}`.")]
    TemplateRenderingFailed(#[from] tera::Error),
}

pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

/// Struct used for storing context during building of configuration
#[derive(Default)]
pub struct ConfigContext {
    warnings: Vec<String>,
}

impl ConfigContext {
    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }
    pub fn get_warnings(&self) -> &Vec<String> {
        &self.warnings
    }
}

/// <!--${internal}-->
/// Main configuration creation trait of mirrord-config
pub trait MirrordConfig {
    /// <!--${internal}-->
    /// The resulting struct you plan on using in the rest of your code
    type Generated;

    /// <!--${internal}-->
    /// Load configuration from all sources and output as [Self::Generated]
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
