use std::path::PathBuf;

use clap::{Args, Subcommand, ValueHint};

/// `mirrord verify` commands.
#[derive(Subcommand, Debug)]
pub(crate) enum VerifyCommand {
    /// Runs a test command and records the run on the cluster (requires operator).
    ///
    /// The record holds the commit that is checked out, and the operator adds every mirrord
    /// session that the command starts. Exits with the command's exit code. When the operator
    /// keeps no verification records, the command still runs and no record is made.
    Run(VerifyRunArgs),

    /// Shows the verification records of a commit, or the record with the given key.
    Show(VerifyLookupArgs),

    /// Exits with 0 when a verification of the commit passed, with 1 when none did, and with 2
    /// when the operator keeps no verification records.
    Check(VerifyLookupArgs),
}

#[derive(Args, Debug)]
pub(crate) struct VerifyArgs {
    /// Command to use with `mirrord verify`.
    #[command(subcommand)]
    pub command: VerifyCommand,
}

#[derive(Args, Debug)]
pub(crate) struct VerifyRunArgs {
    /// Specify config file to use
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// The test command and its arguments, after `--`.
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub(crate) struct VerifyLookupArgs {
    /// Specify config file to use
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// A commit hash, or the key of one record (`verify-...`).
    pub reference: String,
}
