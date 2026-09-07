use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand, ValueHint};

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

    /// Export global configuration as JSON.
    Export(ExportGlobalConfigArgs),

    /// Replace global configuration with JSON.
    Import(ImportGlobalConfigArgs),

    /// Set one or more values addressed by JSON Pointer.
    Set(SetGlobalConfigArgs),

    /// Remove one or more values addressed by JSON Pointer.
    Unset(UnsetGlobalConfigArgs),
}

/// Output accepted by `mirrord global-config export`.
#[derive(Args, Debug)]
pub(crate) struct ExportGlobalConfigArgs {
    /// Write the exported configuration to this file instead of stdout.
    #[arg(long, value_hint = ValueHint::FilePath)]
    pub(crate) file: Option<PathBuf>,
}

/// Input accepted by `mirrord global-config import`.
#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("input")
        .required(true)
        .multiple(false)
        .args(["json", "file"])
))]
pub(crate) struct ImportGlobalConfigArgs {
    /// Global configuration JSON produced by `mirrord global-config export`.
    #[arg(value_name = "JSON")]
    pub(crate) json: Option<String>,

    /// Read global configuration JSON from this file.
    #[arg(long, value_hint = ValueHint::FilePath)]
    pub(crate) file: Option<PathBuf>,
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
