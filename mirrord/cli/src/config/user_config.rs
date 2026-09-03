use clap::{Args, Subcommand};

/// Arguments for `mirrord user-config`.
#[derive(Args, Debug)]
pub(crate) struct UserConfigArgs {
    /// User-wide configuration action to perform.
    #[command(subcommand)]
    pub(crate) command: UserConfigCommand,
}

/// Commands for inspecting and changing user-wide configuration.
#[derive(Debug, Subcommand)]
pub(crate) enum UserConfigCommand {
    /// Print user-wide configuration as JSON.
    Show,

    /// Set one or more values addressed by JSON Pointer.
    Set(SetUserConfigArgs),

    /// Remove one or more values addressed by JSON Pointer.
    Unset(UnsetUserConfigArgs),
}

/// Values accepted by `mirrord user-config set`.
#[derive(Args, Debug)]
pub(crate) struct SetUserConfigArgs {
    /// Assignments in the form `/json/pointer=value`.
    #[arg(required = true, value_name = "POINTER=VALUE")]
    pub(crate) assignments: Vec<String>,
}

/// Values accepted by `mirrord user-config unset`.
#[derive(Args, Debug)]
pub(crate) struct UnsetUserConfigArgs {
    /// JSON Pointers identifying values to remove.
    #[arg(required = true, value_name = "POINTER")]
    pub(crate) pointers: Vec<String>,
}
