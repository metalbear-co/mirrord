use std::{net::SocketAddr, path::PathBuf, str::FromStr};

use kube::core::ErrorResponse;
use miette::Diagnostic;
use mirrord_config::config::ConfigError;
use mirrord_console::error::ConsoleError;
use mirrord_intproxy::{agent_conn::ConnectionTlsError, error::IntProxyError};
use mirrord_kube::error::KubeApiError;
use mirrord_operator::client::error::{HttpError, OperatorApiError, OperatorOperation};
use mirrord_vpn::error::VpnError;
use reqwest::StatusCode;
use thiserror::Error;

use crate::port_forward::PortForwardError;

pub(crate) type Result<T, E = CliError> = core::result::Result<T, E>;

const GENERAL_HELP: &str = r#"

- If you're still stuck:

>> Please open a new bug report at https://github.com/metalbear-co/mirrord/issues/new/choose

>> Or join our Discord https://discord.gg/metalbear and request help in #mirrord-help

>> Or email us at hi@metalbear.co

"#;

const GENERAL_BUG: &str = r#"This is a bug. Please report it in our Discord or GitHub repository.

>> Please open a new bug report at https://github.com/metalbear-co/mirrord/issues/new/choose

>> Or join our Discord https://discord.gg/metalbear and request help in #mirrord-help

>> Or email us at hi@metalbear.co

"#;

/// Errors that can occur when executing the `mirrord container` command.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum ContainerError {
    #[error("Could not serialize config to pass into container: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    ConfigSerialization(serde_json::Error),

    #[error("Could not write serialized config file: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    ConfigWrite(std::io::Error),

    #[error("Could not create a self sigend certificate for proxy: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    SelfSignedCertificate(rcgen::Error),

    #[error("Could not write self sigend certificate for proxy: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    WriteSelfSignedCertificate(std::io::Error),

    #[error("Failed to execute command: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnableToExecuteCommand(std::io::Error),

    #[error("Failed parse command stdout: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnableParseCommandStdout(String, std::io::Error),

    #[error("Command failed to execute command [{0}]: {1}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnsuccesfulCommandOutput(String, String),

    #[error("Failed get running proxy socket addr: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnableParseProxySocketAddr(<SocketAddr as FromStr>::Err),
}

/// Errors that can occur when executing the `mirrord extproxy` command.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum ExternalProxyError {
    #[error("Failed to deserialize connect info `{0}`: {1}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    DeseralizeConnectInfo(String, serde_json::Error),

    #[error("Main internal proxy logic failed: {0}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    Intproxy(#[from] IntProxyError),

    #[error("Failed to set up TCP listener for accepting intproxy connections: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    ListenerSetup(std::io::Error),

    #[error("Failed to open log file at `{0}`: {1}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    OpenLogFile(String, std::io::Error),

    #[error("Failed to set sid: {0}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    SetSid(nix::Error),

    #[error(transparent)]
    #[diagnostic(help("{GENERAL_BUG}"))]
    Tls(#[from] ConnectionTlsError),

    #[error(
        "there was no tls information provided, see `external_proxy` keys in config if specified"
    )]
    #[diagnostic(help("{GENERAL_BUG}"))]
    MissingTlsInfo,
}

/// Errors that can occur when executing the `mirrord intproxy` command.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum InternalProxyError {
    #[error("Failed to set up TCP listener for accepting layer connections: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    ListenerSetup(std::io::Error),

    #[error("Failed to set sid: {0}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    SetSid(nix::Error),

    #[error("Main internal proxy logic failed: {0}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    Intproxy(#[from] IntProxyError),

    #[error("Failed to infer mirrord config: {0}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    Config(#[from] ConfigError),

    #[error("Failed to open log file at `{0}`: {1}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    OpenLogFile(String, std::io::Error),

    #[error("Failed to deserialize connect info `{0}`: {1}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    DeseralizeConnectInfo(String, serde_json::Error),

    #[error("Initial ping pong with the agent failed: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    InitialPingPongFailed(String),
}

/// Errors that can occur when executing the `mirrord operator setup` command.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum OperatorSetupError {
    #[error("Failed to get latest mirrord operator version: {0}")]
    #[diagnostic(help("Please check internet connection.{GENERAL_HELP}"))]
    OperatorVersionCheck(#[from] reqwest::Error),

    #[error("Failed to open output file at `{}`: {1}", .0.display())]
    #[diagnostic(help("{GENERAL_HELP}"))]
    OutputFileOpen(PathBuf, std::io::Error),

    #[error("Failed to write mirrord operator setup: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    SetupWrite(#[from] mirrord_operator::setup::SetupWriteError),
}

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum CliError {
    /// Do not construct this variant directly, use [`CliError::auth_exec_error_or`] to allow for
    /// more granular error detection.
    #[error("Failed to create Kubernetes API client: {0}")]
    #[diagnostic(help("Please check that Kubernetes is configured correctly and test your connection with `kubectl get pods`.{GENERAL_HELP}"))]
    CreateKubeApiFailed(KubeApiError),

    #[error("Failed to list mirrord targets: {0}")]
    #[diagnostic(help("Please check that Kubernetes is configured correctly and test your connection with `kubectl get pods`.{GENERAL_HELP}"))]
    ListTargetsFailed(KubeApiError),

    /// Do not construct this variant directly, use [`CliError::auth_exec_error_or`] to allow for
    /// more granular error detection.
    #[error("Failed to create mirrord-agent: {0}")]
    #[diagnostic(help(
        "Please check the status of the agent pod, using `kubectl get pods` in the relevant namespace. \
        Make sure it is able to fetch the agent image, it didn't fail due to lack of resources, etc.{GENERAL_HELP}"
    ))]
    CreateAgentFailed(KubeApiError),

    /// Do not construct this variant directly, use [`CliError::auth_exec_error_or`] to allow for
    /// more granular error detection.
    #[error("Failed to connect to the created mirrord-agent: {0}")]
    #[diagnostic(help(
        "Please check the following:
    1. The agent is running and the logs are not showing any errors.
    2. (OSS only) You have sufficient permissions to port forward to the agent.{GENERAL_HELP}"
    ))]
    AgentConnectionFailed(KubeApiError),

    #[error("Failed to communicate with the agent: {0}")]
    #[diagnostic(help("Please check agent status and logs.{GENERAL_HELP}"))]
    InitialAgentCommFailed(String),

    #[error("Failed to execute binary `{0}` with args {1:?}")]
    #[diagnostic(help(
        "Please open an issue on our GitHub repository with binary information:
    1. How it was compiled/built.
    2. `file` output on it.
    3. Operating system.
    4. Any extra information you might have.
    5. If you can provide way to build the binary, that would be great.{GENERAL_HELP}"
    ))]
    BinaryExecuteFailed(String, Vec<String>),

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[error("Binary is SIP protected and rosetta is missing")]
    #[diagnostic(help(
        r#"
    The file you are trying to run, `{0}`, is either SIP protected or a script with a
    shebang that leads to a SIP protected binary. In order to bypass SIP protection,
    mirrord creates a non-SIP version of the binary and runs that one instead of the
    protected one. The non-SIP version is however an x86_64 file, so in order to run
    it on apple hardware, rosetta has to be installed.
    Rosetta can be installed by runnning:

    softwareupdate --install-rosetta
    {GENERAL_HELP}
    "#
    ))]
    RosettaMissing(String),

    #[error("Failed to verify mirrord config: {0}")]
    #[diagnostic(help(r#"Inspect your config file and arguments provided.{GENERAL_HELP}"#))]
    ConfigError(#[from] mirrord_config::config::ConfigError),

    #[error("Failed to get canonical path to mirrord config at `{}`: {1}", .0.display())]
    #[diagnostic(help("Please check that the path is correct and that you have permissions to read it.{GENERAL_HELP}"))]
    CanonicalizeConfigPathFailed(PathBuf, std::io::Error),

    #[cfg(target_os = "macos")]
    #[error("SIP Error: `{0:#?}`")]
    #[diagnostic(help(
        r#"This issue is related to SIP on macOS. Please create an issue or consult with us on Discord
        {GENERAL_HELP}"#
    ))]
    SipError(#[from] mirrord_sip::SipError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    OperatorSetupError(#[from] OperatorSetupError),

    #[error("Failed to extract mirrord-layer to `{}`: {1}", .0.display())]
    #[diagnostic(help("{GENERAL_BUG}"))]
    LayerExtractError(PathBuf, std::io::Error),

    #[error("Failed to serialize JSON: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    JsonSerializeError(#[from] serde_json::Error),

    #[error("Failed to connect to mirrord-console: {0}")]
    #[diagnostic(help("Check that mirrord-console is running.{GENERAL_HELP}"))]
    ConsoleConnectError(#[from] ConsoleError),

    #[error("An error ocurred when spawning internal proxy: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    InternalProxySpawnError(String),

    /// Errors produced by `mirrord container` command.
    #[error(transparent)]
    #[diagnostic(transparent)]
    ContainerError(#[from] ContainerError),

    /// Errors produced by `mirrord extproxy` command.
    #[error(transparent)]
    #[diagnostic(transparent)]
    ExternalProxyError(#[from] ExternalProxyError),

    /// Errors produced by `mirrord intproxy` command.
    #[error(transparent)]
    #[diagnostic(transparent)]
    InternalProxyError(#[from] InternalProxyError),

    /// Errors produced by `mirrord vpn` command.
    #[error(transparent)]
    #[diagnostic(help("{GENERAL_HELP}"))]
    VpnError(#[from] VpnError),

    #[error("Getting cli path failed {0:#?}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    CliPathError(std::io::Error),

    #[error("Failed to wait until internal proxy exits: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    InternalProxyWaitError(std::io::Error),

    #[error("Failed to build async runtime: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    RuntimeError(std::io::Error),

    #[error("Feature `{0}` requires using mirrord operator")]
    #[diagnostic(help(
        "The mirrord operator is part of mirrord for Teams. \
        You can get started with mirrord for Teams at this link: https://mirrord.dev/docs/overview/teams/?utm_source=errreqop"
    ))]
    FeatureRequiresOperatorError(String),

    #[error("Feature `{feature}` is not supported in mirrord operator {operator_version}.")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    FeatureNotSupportedInOperatorError {
        feature: String,
        operator_version: String,
    },

    #[error("mirrord operator API failed: {0} failed with {1}")]
    #[diagnostic(help(
    "Please check the following:
    1. The operator is running and the logs are not showing any errors.
    2. You have sufficient permissions to port forward to the operator.

    If you want to run without the operator, please set `\"operator\": false` in the mirrord configuration file.

    Please remember that some features are supported only when using mirrord operator (https://mirrord.dev/docs/overview/teams/#supported-features?utm_source=erropfailed).{GENERAL_HELP}"))]
    OperatorApiFailed(OperatorOperation, kube::Error),

    #[error("mirrord operator rejected {0}: {1}")]
    #[diagnostic(help("If the problem refers to mirrord operator license, visit https://app.metalbear.co to manage or renew your license.{GENERAL_HELP}"))]
    OperatorApiForbidden(OperatorOperation, String),

    #[error(
        "mirrord operator license expired. Visit https://app.metalbear.co to renew your license"
    )]
    #[diagnostic(help("{GENERAL_HELP}"))]
    OperatorLicenseExpired,

    #[error("Failed to build a websocket connect request: {0:#?}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    ConnectRequestBuildError(HttpError),

    #[error("Ping pong with the agent failed: {0}")]
    #[diagnostic(help(
        "This usually means that connectivity was lost while pinging.{GENERAL_HELP}"
    ))]
    PingPongFailed(String),

    #[error("Failed to prepare mirrord operator client certificate: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    OperatorClientCertError(String),

    #[error("mirrord operator was not found in the cluster.")]
    #[diagnostic(help(
        "Command requires the mirrord operator or operator usage was explicitly enabled in the configuration file.
        Read more here: https://mirrord.dev/docs/overview/quick-start/#operator.{GENERAL_HELP}"
    ))]
    OperatorNotInstalled,

    #[error("mirrord returned a target resource of unknown type: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    OperatorReturnedUnknownTargetType(String),

    #[error("An error occurred in the port-forwarding process: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    PortForwardingError(#[from] PortForwardError),

    #[error("Failed to execute authentication command specified in kubeconfig: {0}")]
    #[diagnostic(help("
        mirrord failed to execute Kube authentication command.
        This can happen when the command is not specified using absolute path and cannot be found in $PATH in the context where mirrord is invoked.
        Possible fixes:
        1. Change global $PATH to include the authentication command and relaunch the IDE/terminal.
        2. In the kubeconfig, specify the command with an absolute path.{GENERAL_HELP}
    "))]
    KubeAuthExecFailed(String),
}

impl CliError {
    /// If the given [`KubeApiError`] originates from failed authentication command exec, produces
    /// [`CliError::KubeAuthExecFailed`]. Otherwise, uses the given `fallback` function to
    /// produce the result.
    pub fn auth_exec_error_or<F: FnOnce(KubeApiError) -> Self>(
        error: KubeApiError,
        fallback: F,
    ) -> Self {
        use kube::{client::AuthError, Error};

        match error {
            KubeApiError::KubeError(Error::Auth(AuthError::AuthExec(error))) => {
                Self::KubeAuthExecFailed(error)
            }
            error => fallback(error),
        }
    }
}

impl From<OperatorApiError> for CliError {
    fn from(value: OperatorApiError) -> Self {
        use kube::{client::AuthError, Error};

        match value {
            OperatorApiError::UnsupportedFeature {
                feature,
                operator_version,
            } => Self::FeatureNotSupportedInOperatorError {
                feature: feature.to_string(),
                operator_version,
            },
            OperatorApiError::CreateKubeClient(e) => {
                Self::auth_exec_error_or(e, Self::CreateKubeApiFailed)
            }
            OperatorApiError::ConnectRequestBuildError(e) => Self::ConnectRequestBuildError(e),
            OperatorApiError::KubeError {
                error: Error::Api(ErrorResponse { message, code, .. }),
                operation,
            } if code == StatusCode::FORBIDDEN => Self::OperatorApiForbidden(operation, message),
            OperatorApiError::KubeError {
                error: Error::Auth(AuthError::AuthExec(error)),
                ..
            } => Self::KubeAuthExecFailed(error),
            OperatorApiError::KubeError { error, operation } => {
                Self::OperatorApiFailed(operation, error)
            }
            OperatorApiError::StatusFailure { operation, status }
                if status.code == StatusCode::FORBIDDEN =>
            {
                Self::OperatorApiForbidden(operation, status.message)
            }
            OperatorApiError::StatusFailure { operation, status } => {
                let error = kube::Error::Api(ErrorResponse {
                    status: "Failure".to_string(),
                    message: status.message,
                    reason: status.reason,
                    code: status.code,
                });

                Self::OperatorApiFailed(operation, error)
            }
            OperatorApiError::NoLicense => Self::OperatorLicenseExpired,
            OperatorApiError::ClientCertError(error) => Self::OperatorClientCertError(error),
            OperatorApiError::FetchedUnknownTargetType(error) => {
                Self::OperatorReturnedUnknownTargetType(error.0)
            }
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported runtime version")]
pub struct UnsupportedRuntimeVariant;
