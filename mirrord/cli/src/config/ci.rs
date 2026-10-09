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
    /// - With a Free operator that advertises keyless CI, `MIRRORD_CI_API_KEY` is optional;
    ///   ordinary credentials are automatic.
    /// - With other installations, set `MIRRORD_CI_API_KEY` to a key from `mirrord ci api-key`.
    /// - The operator enforces its license policy for CI credentials.
    /// - Without the operator, no API key is required.
    Start(Box<CiStartArgs>),

    /// Stops mirrord for ci.
    ///
    /// Uses locally saved process state; no API key is required.
    Stop,

    /// Starts mirrord for ci inside a container. Takes the same arguments as `mirrord container`,
    /// plus ci specific options.
    ///
    /// - With a Free operator that advertises keyless CI, `MIRRORD_CI_API_KEY` is optional;
    ///   ordinary credentials are automatic.
    /// - With other installations, set `MIRRORD_CI_API_KEY` to a key from `mirrord ci api-key`.
    /// - The operator enforces its license policy for CI credentials.
    /// - Without the operator, no API key is required.
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

#[cfg(test)]
mod tests {
    use std::ops::Not;

    use clap::{Parser, error::ErrorKind};
    use rstest::rstest;

    use crate::config::Cli;

    #[rstest]
    #[case("start")]
    #[case("container")]
    fn ci_session_help_explains_operator_credential_policy(#[case] subcommand: &str) {
        let error = Cli::try_parse_from(["mirrord", "ci", subcommand, "--help"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();

        assert!(help.contains("Free operator"));
        assert!(help.contains("advertises keyless CI"));
        assert!(help.contains("MIRRORD_CI_API_KEY` is optional"));
        assert!(help.contains("ordinary credentials are automatic"));
        assert!(help.contains("With other installations, set `MIRRORD_CI_API_KEY`"));
        assert!(help.contains("Without the operator, no API key is required"));
    }

    #[test]
    fn ci_stop_help_does_not_require_a_key() {
        let error = Cli::try_parse_from(["mirrord", "ci", "stop", "--help"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();

        assert!(help.contains("no API key is required"));
        assert!(help.contains("MIRRORD_CI_API_KEY").not());
    }
}
