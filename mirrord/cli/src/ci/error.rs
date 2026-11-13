#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use std::time::Duration;

use miette::Diagnostic;
use mirrord_auth::error::ApiKeyError;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub(crate) enum CiError {
    #[error("File operation failed: {0}!")]
    IO(#[from] std::io::Error),

    #[error(transparent)]
    CiApiKey(#[from] ApiKeyError),

    #[error("The environment variable {0} contains an invalid character!")]
    #[diagnostic(help(
        "`mirrord ci start` and `mirrord ci container` use `{0}` for CI credentials; \
         it is optional with supporting Free or Teams operators. CI retains the license tier's \
         features, including with a supplied key. For Free and Teams CI, the operator caps each \
         target-routing loader at one ready pod per participating cluster. Terminating pods may \
         drain while replacements attach. Enterprise and legacy installations require a key from \
         `mirrord ci api-key`. Without the operator, leave it unset. \
         Local `mirrord ci stop` requires no API key."
    ))]
    EnvVar(&'static str, std::env::VarError),

    #[cfg_attr(windows, allow(unused))]
    #[error("Failed to execute binary `{0}` with args {1:?}")]
    BinaryExecuteFailed(String, Vec<String>),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    #[error("another mirrord process held the mirrord ci session's lock for more than {timeout:?}")]
    #[diagnostic(help(
        "`mirrord ci stop` stops every mirrord ci session on this machine, including sessions \
         that are still starting, so `mirrord ci stop` in another CI job on the same runner can \
         reach this session. That stop holds the session's lock while it stops the session, and \
         stopping a `mirrord ci container` session can take up to 10 seconds, which is longer \
         than this process waits for the lock. Run this CI job again once that stop is done."
    ))]
    SessionLocked { timeout: Duration },

    /// Lists every session that `mirrord ci stop` could not stop, with the reason, including the
    /// sessions it skipped because another mirrord process held their lock. Without interactive
    /// progress output, as in a CI log, this error is all that the user sees of the stop.
    #[cfg(not(target_os = "windows"))]
    #[error(
        "`mirrord ci stop` stopped {} of the {total} mirrord ci sessions it found, and could not \
         stop these:{}{}",
        .total - .failures.len() - .skipped.len(),
        .failures
            .iter()
            .map(|(session_dir, error)| format!("\n- {}: {error}", session_dir.display()))
            .collect::<String>(),
        .skipped
            .iter()
            .map(|session_dir| format!(
                "\n- {}: skipped, another mirrord process held the session's lock; run \
                 `mirrord ci stop` again once that process is done",
                session_dir.display()
            ))
            .collect::<String>()
    )]
    #[diagnostic(help(
        "Run `mirrord ci stop` again to retry the sessions that it could not stop."
    ))]
    SessionsNotStopped {
        total: usize,
        failures: Vec<(PathBuf, CiError)>,
        /// Sessions whose lock another mirrord process held, so the stop left them alone.
        skipped: Vec<PathBuf>,
    },

    #[error(
        "`MIRRORD_CI_API_KEY` is required for operator-backed `mirrord ci start` and `mirrord ci container` when the operator does not advertise keyless CI."
    )]
    #[diagnostic(help(
        "Supporting Free or Teams operators advertise keyless CI and use automatic ordinary \
         credentials without a CI key. CI retains the license tier's features, including with a \
         supplied key. For Free and Teams CI, the operator caps each target-routing loader at one \
         ready pod per participating cluster. Terminating pods may drain while replacements \
         attach. Enterprise and legacy installations require this environment variable to contain \
         a key from `mirrord ci api-key`. \
         Without the operator, no CI API key is required. Local `mirrord ci stop` requires no API key."
    ))]
    MissingCiApiKey,

    #[cfg(not(target_os = "windows"))]
    #[error("`mirrord ci` failed to execute command with `{0}`!")]
    #[diagnostic(help(
        "`mirrord ci` failed to execute an internal command for this operation, please report it to us."
    ))]
    NixErrno(#[from] nix::errno::Errno),

    #[cfg(not(target_os = "windows"))]
    #[error("`mirrord ci container` runtime command `{command}` failed with {message}")]
    ContainerRuntimeCommand { command: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ci_api_key_guides_start_and_container() {
        let error = CiError::MissingCiApiKey;
        let message = error.to_string();
        assert!(message.contains("operator-backed"));
        assert!(message.contains("mirrord ci start"));
        assert!(message.contains("mirrord ci container"));
        assert!(message.contains("does not advertise keyless CI"));

        let help = error.help().unwrap().to_string();
        assert!(help.contains("mirrord ci api-key"));
        assert!(help.contains("Supporting Free or Teams operators"));
        assert!(help.contains("advertise keyless CI"));
        assert!(help.contains("CI retains the license tier's features"));
        assert!(
            help.contains("each target-routing loader at one ready pod per participating cluster")
        );
        assert!(help.contains("Terminating pods may drain while replacements attach"));
        assert!(help.contains("including with a supplied key"));
        assert!(help.contains("Without the operator, no CI API key is required"));
        assert!(help.contains("mirrord ci stop` requires no API key"));
    }
}
