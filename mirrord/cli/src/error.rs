use std::path::PathBuf;

use miette::Diagnostic;
use mirrord_console::error::ConsoleError;
use mirrord_kube::error::KubeApiError;
use mirrord_operator::client::OperatorApiError;
use thiserror::Error;

pub(crate) type Result<T, E = CliError> = miette::Result<T, E>;

const GENERAL_HELP: &str = r#"

- If you're still stuck:

>> Please open a new bug report at https://github.com/metalbear-co/mirrord/issues/new/choose

>> Or join our Discord https://discord.gg/metalbear and request help in #mirrord-help

>> Or email us at hi@metalbear.co

"#;

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum InternalProxyError {
    #[error("Couldn't listen for connections {0:#?}")]
    ListenError(std::io::Error),
    #[error("Couldn't get local port{0:#?}")]
    LocalPortError(std::io::Error),
    #[error("Couldn't accept connection before timeout")]
    FirstConnectionTimeout,
    #[error("Couldn't accept connection {0:#?}")]
    AcceptError(std::io::Error),
    #[error("Couldn't connect to agent via TCP {0:#?}")]
    TcpConnectError(std::io::Error),
    #[error("Agent closed connection on ping/pong, image version/arch mismatch?")]
    AgentClosedConnection,
    #[error("Ping error {0:#?} - image version/arch mismatch?")]
    PingError(#[from] tokio::sync::mpsc::error::SendError<mirrord_protocol::ClientMessage>),
    #[error("Set sid failed {0:#?}, please report a bug")]
    SetSidError(nix::Error),
    #[error("No connection method, please report a bug")]
    NoConnectionMethod,
}

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum CliError {
    #[error("Failed to connect to the operator. We have found the operator and unable to connect to it. {0:#?}")]
    #[diagnostic(help(
        r#"
    Please check the following:
    1. The operator is running and the logs are not showing any errors.
    2. You have sufficient permissions to port forward to the operator.
    {GENERAL_HELP}"#
    ))]
    OperatorConnectionFailed(#[from] OperatorApiError),
    #[error("Failed to connect to the operator. Someone else is stealing traffic from the requested target")]
    #[diagnostic(help(
        r#"
    If you want to run anyway please set the following:
    
    {{
      "feature": {{
        "network": {{
          "incoming": {{
            ...
            "on_concurrent_steal": "continue" // or "override"
          }}
        }}
      }}
    }}

    More info (https://mirrord.dev/docs/overview/configuration/#feature-network-incoming-on_concurrent_steal)

    {GENERAL_HELP}"#
    ))]
    OperatorConcurrentSteal,
    #[error("Failed to create Kubernetes API. {0:#?}")]
    #[diagnostic(help(
        r#"
    Please check that Kubernetes is configured correctly.
    Test your connection with `kubectl get pods`.
    {GENERAL_HELP}"#
    ))]
    KubernetesApiFailed(#[from] KubeApiError),
    #[error("Agent wasn't ready in time")]
    #[diagnostic(help(
        r#"
    Please check the status of the agent pod, using `kubectl get pods` in the relevant namespace.
    Make sure it is able to fetch the agent image, it didn't fail due to lack of resources, etc.
    {GENERAL_HELP}"#
    ))]
    AgentReadyTimeout,
    #[error("Create agent failed. {0:#?}")]
    #[diagnostic(help(
        r#"
    Please check the status of the agent pod, using `kubectl get pods` in the relevant namespace.
    Make sure it is able to fetch the agent image, it didn't fail due to lack of resources, etc.
    {GENERAL_HELP}"#
    ))]
    CreateAgentFailed(KubeApiError),
    #[error("Failed to connect to the created agent. {0:#?}")]
    #[diagnostic(help(
        r#"
    Please check the following:
    1. The agent is running and the logs are not showing any errors.
    2. You have sufficient permissions to port forward to the agent.
    {GENERAL_HELP}"#
    ))]
    AgentConnectionFailed(KubeApiError),
    #[error("Invalid environment configuration. Include {0:#?} and exclude {1:#?}")]
    #[diagnostic(help(
        r#"
    mirrord doesn't support specifying both
    `OVERRIDE_ENV_VARS_EXCLUDE` and `OVERRIDE_ENV_VARS_INCLUDE` at the same time!

    > Use either `--override_env_vars_exclude` or `--override_env_vars_include`.
    >> If you want to include all, use `--override_env_vars_include="*"`.
    {GENERAL_HELP}"#
    ))]
    InvalidEnvConfig(String, String),
    #[error("Invalid message received from agent {0:#?}")]
    #[diagnostic(help(
        "This is a bug. Please report it in our Discord or GitHub repository. {GENERAL_HELP}"
    ))]
    InvalidMessage(String),
    #[error("Initial communication with the agent failed. {0:#?}")]
    #[diagnostic(help(
        "Please make sure the agent is running and the logs are not showing any errors.{GENERAL_HELP}"
    ))]
    InitialCommFailed(&'static str),
    #[error("Failed to execute binary `{0:#?}` with args `{1:#?}`")]
    #[diagnostic(help(
        r#"
    Please open an issue on our GitHub repository with binary information:
    1. How it was compiled/built.
    2. `file` output on it.
    3. Operating system
    4. Any extra information you might have.
    5. If you can provide way to build the binary, that would be great.
    {GENERAL_HELP}"#
    ))]
    BinaryExecuteFailed(String, Vec<String>),
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[error("Binary is SIP protected and rosetta is missing")]
    #[diagnostic(help(
        r#"
    The file you are trying to run, `{0:#?}`, is either SIP protected or a script with a
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
    #[error("Configuration file parsing failed - {0:#?}")]
    #[diagnostic(help(
        r#"Configuration file parsing failed. Inspect your config file or arguments provided.{GENERAL_HELP}"#
    ))]
    ConfigError(#[from] mirrord_config::config::ConfigError),
    #[error("Error with config file's path at `{0:#?}`: `{1:#?}`")]
    #[diagnostic(help(
        "Please check that the path is correct and that you have permissions to read it.{GENERAL_HELP}",
    ))]
    ConfigFilePathError(PathBuf, std::io::Error),
    #[error("Creating kubernetes manifest yaml file failed with err : {0:#?}")]
    #[diagnostic(help(
        r#"Check if you have permissions to write to the file and/or directory exists{GENERAL_HELP}"#
    ))]
    ManifestFileError(std::io::Error),
    #[cfg(target_os = "macos")]
    #[error("SIP Error: `{0:#?}`")]
    #[diagnostic(help(
        r#"This issue is related to SIP on macOS. Please create an issue or consult with us on Discord
        {GENERAL_HELP}"#
    ))]
    SipError(#[from] mirrord_sip::SipError),
    #[error("Operator setup error: `{0:#?}`")]
    SetupError(#[from] mirrord_operator::setup::SetupError),
    #[error("Error extracting layer to `{0:#?}`: `{1:#?}`")]
    #[diagnostic(help("Please report a bug.{GENERAL_HELP}",))]
    LayerExtractFailed(PathBuf, std::io::Error),
    #[error("JSON Serialization error: `{0:#?}`")]
    JsonSerializeError(#[from] serde_json::Error),
    #[error("Failed connecting to mirrord console for logging {0:#?}")]
    ConsoleConnectError(#[from] ConsoleError),
    #[error("Couldn't get stdout of internal proxy")]
    #[diagnostic(help(
        r#"This is a bug. Please report it in our Discord or GitHub repository. {GENERAL_HELP}"#
    ))]
    InternalProxyStdoutError,
    #[error("Couldn't get port of internal proxy")]
    #[diagnostic(help(
        r#"This is a bug. Please report it in our Discord or GitHub repository. {GENERAL_HELP}"#
    ))]
    InternalProxyPortReadError,
    #[error("Internal proxy read error: {0:#?}")]
    InternalProxyReadError(std::io::Error),
    #[error("Internal proxy error: {0:#?}")]
    InternalProxyError(#[from] InternalProxyError),
    #[error("Getting cli path failed {0:#?}")]
    CliPathError(std::io::Error),
    #[error("Executing internal proxy failed {0:#?}")]
    InternalProxyExecutionFailed(std::io::Error),
    #[error("Internal proxy port parse error: {0:#?}")]
    InternalProxyPortParseError(std::num::ParseIntError),
    #[error("Internal proxy wait error: {0:#?}")]
    InternalProxyWaitError(std::io::Error),
    #[error("Waitlist registration failed.")]
    #[diagnostic(help("Please check the email provided and internet connection.{GENERAL_HELP}"))]
    WaitlistError(reqwest::Error),
}
