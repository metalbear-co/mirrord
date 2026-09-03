use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand, ValueHint};

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

    /// Export portable user-wide configuration as JSON.
    Export(ExportUserConfigArgs),

    /// Replace user-wide configuration with portable JSON.
    Import(ImportUserConfigArgs),

    /// Set one or more values addressed by JSON Pointer.
    Set(SetUserConfigArgs),

    /// Remove one or more values addressed by JSON Pointer.
    Unset(UnsetUserConfigArgs),
}

/// Output accepted by `mirrord user-config export`.
#[derive(Args, Debug)]
pub(crate) struct ExportUserConfigArgs {
    /// Write the exported configuration to this file instead of stdout.
    #[arg(long, value_hint = ValueHint::FilePath)]
    pub(crate) file: Option<PathBuf>,
}

/// Input accepted by `mirrord user-config import`.
#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("input")
        .required(true)
        .multiple(false)
        .args(["json", "file"])
))]
pub(crate) struct ImportUserConfigArgs {
    /// User-wide configuration JSON produced by `mirrord user-config export`.
    #[arg(value_name = "JSON")]
    pub(crate) json: Option<String>,

    /// Read user-wide configuration JSON from this file.
    #[arg(long, value_hint = ValueHint::FilePath)]
    pub(crate) file: Option<PathBuf>,
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
