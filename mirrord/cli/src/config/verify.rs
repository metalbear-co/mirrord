use std::path::PathBuf;

use clap::{Args, Subcommand, ValueHint};

/// `mirrord verify` commands.
#[derive(Subcommand, Debug)]
pub(crate) enum VerifyCommand {
    /// Runs a test command and records the run on the cluster (requires operator).
    ///
    /// The record holds the commit that is checked out, and the operator adds every mirrord
    /// session that the command starts and the traffic it routes to them. Exits with the
    /// command's exit code. When the operator keeps no verification records, the command still
    /// runs and no record is made.
    Run(VerifyRunArgs),

    /// Shows the verification records of a commit, or the record with the given key.
    Show(VerifyLookupArgs),

    /// Exits with 0 when a verification of the commit passed, with 1 when none did, and with 2
    /// when the operator keeps no verification records.
    Check(VerifyLookupArgs),

    /// Prints a signed verification as markdown for a pull request comment, with the statement
    /// the operator signed in it, so that it can be checked without access to the cluster. For
    /// a commit with several records, prints one that passed if there is one.
    Receipt(VerifyLookupArgs),
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

    /// A YAML or JSON file with checks: traffic that the operator must have routed to a session
    /// of the command, such as an HTTP request that a session stole and answered with a given
    /// status, or messages of a queue. The run passes only when the operator saw all of it.
    #[arg(long, value_hint = ValueHint::FilePath)]
    pub checks: Option<PathBuf>,

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
