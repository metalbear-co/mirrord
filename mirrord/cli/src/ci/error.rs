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
         it is optional when a Free operator advertises keyless CI. Other installations \
         require a key. Set it to the value from \
         `mirrord ci api-key` when required. Without the operator, leave it unset. \
         Local `mirrord ci stop` requires no API key."
    ))]
    EnvVar(&'static str, std::env::VarError),

    #[cfg_attr(windows, allow(unused))]
    #[error("Failed to execute binary `{0}` with args {1:?}")]
    BinaryExecuteFailed(String, Vec<String>),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    #[error(
        "`MIRRORD_CI_API_KEY` is required for operator-backed `mirrord ci start` and `mirrord ci container` when the operator does not advertise keyless CI."
    )]
    #[diagnostic(help(
        "Set this environment variable to the value received from `mirrord ci api-key`. \
         Supporting Free operators advertise keyless CI and use automatic ordinary credentials. \
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
        assert!(help.contains("Free operators advertise keyless CI"));
        assert!(help.contains("Without the operator, no CI API key is required"));
        assert!(help.contains("mirrord ci stop` requires no API key"));
    }
}
