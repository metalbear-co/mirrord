use clap::{Args, Subcommand};

/// Arguments for `mirrord global-config`.
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

    /// Set one or more values addressed by JSON Pointer.
    Set(SetGlobalConfigArgs),

    /// Remove one or more values addressed by JSON Pointer.
    Unset(UnsetGlobalConfigArgs),
}

/// Values accepted by `mirrord global-config set`.
#[derive(Args, Debug)]
pub(crate) struct SetGlobalConfigArgs {
    /// Assignments in the form `/json/pointer=value`.
    #[arg(required = true, value_name = "POINTER=VALUE")]
    pub(crate) assignments: Vec<String>,
}

/// Values accepted by `mirrord global-config unset`.
#[derive(Args, Debug)]
pub(crate) struct UnsetGlobalConfigArgs {
    /// JSON Pointers identifying values to remove.
    #[arg(required = true, value_name = "POINTER")]
    pub(crate) pointers: Vec<String>,
}
