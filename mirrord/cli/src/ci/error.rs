use miette::Diagnostic;
use mirrord_auth::error::ApiKeyError;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub(crate) enum CiError {
    #[error("File operation failed: {0}!")]
    IO(#[from] std::io::Error),

    #[error(transparent)]
    CiApiKey(#[from] ApiKeyError),

    #[error(
        "The required environment variable {0} was not found or contains an invalid character!"
    )]
    #[diagnostic(help(
        "`mirrord ci start` and `mirrord ci stop` require the environment variable `{0}` to be set, \
         please add the missing env var before trying to run the `mirrord ci` command again."
    ))]
    EnvVar(&'static str, std::env::VarError),

    #[error("mirrord-intproxy may be running already!")]
    IntproxyPidAlreadyPresent,

    #[error("mirrord user process may be running already!")]
    UserPidAlreadyPresent,

    #[cfg_attr(windows, allow(unused))]
    #[error("`mirrord ci stop` could not retrieve the mirrord-intproxy pid!")]
    #[diagnostic(help(
        "`mirrord ci stop` reads the file `/tmp/mirrord/mirrord-for-ci-intproxy-pid` to stop \
        the running mirrord session, and we could not retrieve this pid. You can manually stop mirrord \
        by searching for the pid with `ps | grep mirrord` and calling `kill [pid]`."
    ))]
    #[cfg(not(target_os = "windows"))]
    IntproxyPidMissing,

    #[cfg_attr(windows, allow(unused))]
    #[error("`mirrord ci stop` could not retrieve the mirrord-intproxy pid!")]
    #[diagnostic(help(
        "`mirrord ci stop` reads the file `/tmp/mirrord/mirrord-for-ci-intproxy-pid` to stop \
        the running mirrord session, and we could not retrieve this pid. You can manually stop mirrord \
        by searching for the pid with `ps | grep mirrord` and calling `kill [pid]`."
    ))]
    #[cfg(not(target_os = "windows"))]
    UserPidMissing,

    #[cfg_attr(windows, allow(unused))]
    #[error("Failed to execute binary `{0}` with args {1:?}")]
    BinaryExecuteFailed(String, Vec<String>),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    #[error("`MIRRORD_CI_API_KEY` env var is missing!")]
    #[diagnostic(help(
        "`mirrord ci start` requires this env var when running with the mirrord operator to avoid \
        creating invalid credentials. \
        Please add this env var with the value received from `mirrord ci api-key`."
    ))]
    MissingCiApiKey,

    #[cfg(not(target_os = "windows"))]
    #[error("`mirrord ci` failed to execute command with `{0}`!")]
    #[diagnostic(help(
        "`mirrord ci` failed to execute an internal command for this operation, please report it to us."
    ))]
    NixErrno(#[from] nix::errno::Errno),

    #[error(transparent)]
    Config(#[from] mirrord_config::config::ConfigError),

    #[error(transparent)]
    OperatorApi(#[from] mirrord_operator::client::error::OperatorApiError),
}
