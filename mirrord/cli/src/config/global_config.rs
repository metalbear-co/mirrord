use clap::{Args, Subcommand};

/// Arguments for `mirrord config`.
#[derive(Args, Debug)]
pub(crate) struct GlobalConfigArgs {
    /// Global configuration action to perform.
    #[command(subcommand)]
    pub(crate) command: GlobalConfigCommand,
}

/// Commands for inspecting and changing global configuration.
#[derive(Debug, Subcommand)]
pub(crate) enum GlobalConfigCommand {
    /// Print global configuration as JSON.
    Show,

    /// Set a value addressed by a dotted field path.
    Set(SetGlobalConfigArgs),

    /// Remove a value addressed by a dotted field path.
    Unset(UnsetGlobalConfigArgs),
}

/// Values accepted by `mirrord config set`.
///
/// Example:
///
/// ```sh
/// mirrord config set operator 'true'
/// mirrord config set agent.image 'custom.image/latest'
/// ```
#[derive(Args, Debug)]
pub(crate) struct SetGlobalConfigArgs {
    /// Dotted path to a configuration field, for example `agent.image`.
    #[arg(value_name = "PATH")]
    pub(crate) path: String,

    /// JSON value, or a raw string, for example `'custom.image/latest'`.
    #[arg(value_name = "VALUE", allow_hyphen_values = true)]
    pub(crate) value: String,
}

/// Values accepted by `mirrord config unset`.
///
/// Example:
///
/// ```sh
/// mirrord config unset operator
/// mirrord config unset agent.image
/// ```
#[derive(Args, Debug)]
pub(crate) struct UnsetGlobalConfigArgs {
    /// Dotted path to a configuration field, for example `agent.image`.
    #[arg(value_name = "PATH")]
    pub(crate) path: String,
}
