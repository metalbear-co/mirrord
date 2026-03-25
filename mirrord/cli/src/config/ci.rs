use std::path::PathBuf;

use clap::{Args, Subcommand, ValueHint};

use crate::{ContainerArgs, ExecArgs};

/// `mirrord ci` commands.
#[derive(Subcommand, Debug)]
pub(crate) enum CiCommand {
    /// Generates a `CiApiKey` that should be set in the ci's environment variable as
    /// `MIRRORD_CI_API_KEY`.
    ApiKey {
        /// Specify config file to use
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
        config_file: Option<PathBuf>,
    },

    /// Starts mirrord for ci. Takes the same arguments as `mirrord exec` plus ci specific options.
    ///
    /// - The environment variable `MIRRORD_CI_API_KEY` must be set for this command to work.
    Start(Box<CiStartArgs>),

    /// Stops mirrord for ci.
    ///
    /// - The environment variable `MIRRORD_CI_API_KEY` must be set for this command to work.
    Stop,

    /// Starts mirrord for ci inside a container. Takes the same arguments as `mirrord container`,
    /// plus ci specific options.
    ///
    /// - The environment variable `MIRRORD_CI_API_KEY` must be set for this command to work.
    Container(Box<CiContainerArgs>),
}

#[derive(Args, Debug)]
pub(crate) struct CiArgs {
    /// Command to use with `mirrord ci`.
    #[command(subcommand)]
    pub command: CiCommand,
}

/// mirrord for ci args that are the same for the commands that start a session.
#[derive(Args, Debug, Default, Clone)]
pub(crate) struct CiCommonArgs {
    /// Runs mirrord ci in the foreground (the default behaviour is to run it as a background
    /// task).
    #[arg(long)]
    pub foreground: bool,

    /// CI environment, e.g. "staging", "production", "testing", etc.
    #[arg(long)]
    pub environment: Option<String>,

    /// CI pipeline or job name, e.g. "e2e-tests".
    #[arg(long)]
    pub pipeline: Option<String>,

    /// CI pipeline trigger, e.g. "push", "pull request", "manual", etc.
    #[arg(long)]
    pub triggered_by: Option<String>,
}

// `mirrord ci start` command
#[derive(Args, Debug)]
pub(crate) struct CiStartArgs {
    /// Args passed down to mirrord itself (similar to `mirrord exec`).
    #[clap(flatten)]
    pub exec_args: Box<ExecArgs>,

    /// mirrord for ci args.
    #[clap(flatten)]
    pub ci_common_args: CiCommonArgs,
}

/// `mirrord ci container` command
#[derive(Args, Debug)]
pub(crate) struct CiContainerArgs {
    /// Args passed down to mirrord itself (similar to `mirrord container`).
    #[clap(flatten)]
    pub container_args: Box<ContainerArgs>,

    /// mirrord for ci args.
    #[clap(flatten)]
    pub ci_common_args: CiCommonArgs,
}
