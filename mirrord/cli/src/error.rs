use miette::Diagnostic;
use mirrord_kube::error::KubeApiError;
use thiserror::Error;

pub(crate) type Result<T, E = CliError> = miette::Result<T, E>;

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum CliError {
    #[error("Failed to connect to the operator. We have found the operator and unable to connect to it. {0}")]
    #[diagnostic(help(
        r#"
    Please check the following:
    1. The operator is running and the logs are not showing any errors.
    2. You have sufficient permissions to port forward to the operator.
    "#
    ))]
    OperatorConnectionFailed(KubeApiError),
    #[error("Failed to create Kubernetes API. {0}")]
    #[diagnostic(help(
        r#"
    Please check that Kubernetes is configured correctly.
    Test your connection with `kubectl get pods`.
    "#
    ))]
    KubernetesAPIFailed(KubeApiError),
    #[error("Agent wasn't ready in time")]
    #[diagnostic(help(
        r#"
    Please check the status of the agent pod, using `kubectl get pods` in the relevant namespace.
    Make sure it is able to fetch the agent image, it didn't fail due to lack of resources, etc.
    "#
    ))]
    AgentReadyTimeout,
    #[error("Create agent failed. {0}")]
    #[diagnostic(help(
        r#"
    Please check the status of the agent pod, using `kubectl get pods` in the relevant namespace.
    Make sure it is able to fetch the agent image, it didn't fail due to lack of resources, etc.
    "#
    ))]
    CreateAgentFailed(KubeApiError),
    #[error("Failed to connect to the created agent. {0}")]
    #[diagnostic(help(
        r#"
    Please check the following:
    1. The agent is running and the logs are not showing any errors.
    2. You have sufficient permissions to port forward to the agent.
    "#
    ))]
    AgentConnectionFailed(KubeApiError),
    #[error("Invalid environment configuration. Include {0} and exclude {1}")]
    #[diagnostic(help(
        r#"
    mirrord doesn't support specifying both
    `OVERRIDE_ENV_VARS_EXCLUDE` and `OVERRIDE_ENV_VARS_INCLUDE` at the same time!

    > Use either `--override_env_vars_exclude` or `--override_env_vars_include`.
    >> If you want to include all, use `--override_env_vars_include="*"`."#
    ))]
    InvalidEnvConfig(String, String),
    #[error("Invalid message received from agent {0}")]
    #[diagnostic(help("This is a bug. Please report it in our Discord or GitHub repository."))]
    InvalidMessage(String),
    #[error("Timeout waiting for remote environment variables.")]
    #[diagnostic(help(
        "Please make sure the agent is running and the logs are not showing any errors."
    ))]
    GetEnvironmentTimeout,
    #[error("Failed to execute binary `{0}` with args `{1}`")]
    #[diagnostic(help(
        r#"
    Please open an issue on our GitHub repository with binary information:
    1. How it was compiled/built.
    2. `file` output on it.
    3. Operating system
    4. Any extra information you might have.
    5. If you can provide way to build the binary, that would be great.
    "#
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

    "#
    ))]
    RosettaMissing(String),
    #[error("Configuration file parsing failed - {0}")]
    #[diagnostic(help(
        r#"Configuration file parsing failed. Inspect your config file or arguments provided."#
    ))]
    ConfigError(#[from] mirrord_config::config::ConfigError),
}
