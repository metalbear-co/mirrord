use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub(crate) enum VerifyError {
    #[error("Failed to run `git`: {0}")]
    #[diagnostic(help(
        "`mirrord verify run` records the commit that is checked out, so it needs `git` on the PATH."
    ))]
    GitNotRun(std::io::Error),

    #[error("`git {args}` failed: {stderr}")]
    #[diagnostic(help(
        "Run `mirrord verify run` inside a git checkout that has at least one commit."
    ))]
    Git { args: String, stderr: String },

    #[error("No command was given to run")]
    EmptyCommand,

    #[error("Failed to run `{command}`: {error}")]
    CommandNotRun {
        command: String,
        error: std::io::Error,
    },

    #[error("Failed to wait for the command: {0}")]
    Wait(std::io::Error),

    #[error("Failed to read the checks in `{}`: {reason}", path.display())]
    #[diagnostic(help(
        "The file is a YAML or JSON list. Each check has `name`, `namespace`, `service`, `port` and `path`, and optionally `method`, `expectStatus` and `expectBodyContains`."
    ))]
    ChecksFile {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error("{0}")]
    Signature(&'static str),

    #[error("`{0}` is not a full commit hash")]
    #[diagnostic(help(
        "`mirrord verify check` takes the full 40-character hash, so that a short one can't match another commit."
    ))]
    NotAFullCommit(String),

    #[error("No verification record found for `{0}`")]
    NoRecord(String),

    #[error("Failed to read verification records: {0}")]
    Kube(#[from] kube::Error),
}
