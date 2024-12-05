use std::{ffi::NulError, net::SocketAddr, num::ParseIntError, path::PathBuf, str::FromStr};

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

pub(crate) type CliResult<T, E = CliError> = core::result::Result<T, E>;

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

    #[error("Failed read command stdout: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnableReadCommandStdout(String, std::io::Error),

    #[error("Failed read command stderr: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnableReadCommandStderr(String, std::io::Error),

    #[error("Command failed to execute command [{0}]: {1}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UnsuccesfulCommandExecute(String, String),

    #[error("Command output indicates an error [{0}]: {1}")]
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

    #[error("Failed to open output file at `{0}`: {1}")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    OutputFileOpen(PathBuf, std::io::Error),

    #[error("Failed to write mirrord operator setup: {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    SetupWrite(#[from] mirrord_operator::setup::SetupWriteError),
}

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum CliError {
    /// Do not construct this variant directly, use [`CliError::friendlier_error_or_else`] to allow
    /// for more granular error detection.
    #[error("Failed to create Kubernetes API client: {0}")]
    #[diagnostic(help("Please check that Kubernetes is configured correctly and test your connection with `kubectl get pods`.{GENERAL_HELP}"))]
    CreateKubeApiFailed(KubeApiError),

    #[error("Failed to list mirrord targets: {0}")]
    #[diagnostic(help("Please check that Kubernetes is configured correctly and test your connection with `kubectl get pods`.{GENERAL_HELP}"))]
    ListTargetsFailed(KubeApiError),

    /// Do not construct this variant directly, use [`CliError::friendlier_error_or_else`] to allow
    /// for more granular error detection.
    #[error("Failed to create mirrord-agent: {0}")]
    #[diagnostic(help(
        r"1. Please check the status of the agent pod, using `kubectl get pods` in the relevant namespace.
        2. If you don't see any `mirrord-agent-[...]` pods, then try running `kubectl get jobs`, and `kubectl describe mirrord-agent-[...]`.
        Make sure it is able to fetch the agent image, it didn't fail due to lack of resources, etc.{GENERAL_HELP}"
    ))]
    CreateAgentFailed(KubeApiError),

    /// Do not construct this variant directly, use [`CliError::friendlier_error_or_else`] to allow
    /// for more granular error detection.
    #[error("Failed to connect to the created mirrord-agent: {0}")]
    #[diagnostic(help(
        "Please check the following:
    1. The agent is running and the logs are not showing any errors.
    2. (OSS only) You have sufficient permissions to port forward to the agent.{GENERAL_HELP}"
    ))]
    AgentConnectionFailed(KubeApiError),

    /// Friendlier version of the invalid certificate error that comes from a
    /// [`kube::Error::Service`].
    #[error("Kube API operation failed due to missing or invalid certificate: {0}")]
    #[diagnostic(help(
        "Consider enabling `accept_invalid_certificates` in your \
        `mirrord.json`, or running `mirrord exec` with the `-c` flag."
    ))]
    InvalidCertificate(KubeApiError),

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

    #[error("Failed to get canonical path to mirrord config at `{0}`: {1}")]
    #[diagnostic(help("Please check that the path is correct and that you have permissions to read it.{GENERAL_HELP}"))]
    CanonicalizeConfigPathFailed(PathBuf, std::io::Error),

    #[error("Failed to access env file at `{0}`: {1}")]
    #[diagnostic(help("Please check that the path is correct and that you have permissions to read it.{GENERAL_HELP}"))]
    EnvFileAccessError(PathBuf, std::io::Error),

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

    #[error("`mirrord operator status` command failed! Could not retrieve operator status API.")]
    #[diagnostic(help("{GENERAL_HELP}"))]
    OperatorStatusNotFound,

    #[error("Failed to extract mirrord-layer to `{0}`: {1}")]
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
        You can get started with mirrord for Teams at this link: https://mirrord.dev/docs/overview/teams/?utm_source=errreqop&utm_medium=cli"
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

    Please remember that some features are supported only when using mirrord operator (https://mirrord.dev/docs/overview/teams/#supported-features?utm_source=erropfailed&utm_medium=cli).{GENERAL_HELP}"))]
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

    #[error("Failed to make secondary agent connection: {0}")]
    #[diagnostic(help("Please check that Kubernetes is configured correctly and test your connection with `kubectl get pods`.{GENERAL_HELP}"))]
    PortForwardingSetupError(KubeApiError),

    #[error("Failed to make secondary agent connection: invalid configuration, could not find method for connection")]
    PortForwardingNoConnectionMethod,

    #[error("Failed to make secondary agent connection (TLS): {0}")]
    AgentConnTlsError(#[from] ConnectionTlsError),

    #[error("An error occurred in the port-forwarding process: {0}")]
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

    #[error("Failed while resolving target while using the mirrord-operator: {0}")]
    #[diagnostic(help(
        "
        mirrord failed to resolve or validate a target.
        Target resolution failure happens when the target cannot be found, or doesn't exist.
        Validation may fail for a variety of reasons, such as: target is in an invalid state, or missing required fields.
        Please check that your Kubernetes user has access to the target, and that the target actually exists in the cluster.
    "
    ))]
    OperatorTargetResolution(KubeApiError),

    #[error("A null byte was found when trying to execute process: {0}")]
    ExecNulError(#[from] NulError),

    #[error("Couldn't resolve binary name '{0}': {1}")]
    BinaryWhichError(String, String),

    #[error(transparent)]
    ParseInt(ParseIntError),
}

impl CliError {
    /// Here we give more meaning to some errors, instead of just letting them pass as
    /// whatever [`KubeApiError`] we're getting.
    ///
    /// If `error` is not something we're interested in (no need for a special diagnostic message),
    /// then we turn it into a `fallback` [`CliError`].
    pub fn friendlier_error_or_else<F: FnOnce(KubeApiError) -> Self>(
        error: KubeApiError,
        fallback: F,
    ) -> Self {
        use kube::{client::AuthError, Error};

        match error {
            KubeApiError::KubeError(Error::Auth(AuthError::AuthExec(error))) => {
                Self::KubeAuthExecFailed(error.to_owned())
            }
            // UGH(alex): Type-erased errors are messy, and this one is especially bad.
            // See `kube_service_error_dependency_is_in_sync` for a "what's going on here".
            KubeApiError::KubeError(Error::Service(ref fail))
                if format!("{fail:?}").contains("InvalidCertificate") =>
            {
                Self::InvalidCertificate(error)
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
                Self::friendlier_error_or_else(e, Self::CreateKubeApiFailed)
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
            OperatorApiError::KubeApi(error) => Self::OperatorTargetResolution(error),
            OperatorApiError::ParseInt(error) => Self::ParseInt(error),
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported runtime version")]
pub struct UnsupportedRuntimeVariant;

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        sync::Arc,
    };

    use http_body_util::Full;
    use hyper::{
        body::{Bytes, Incoming},
        service::service_fn,
        Request, Response,
    };
    use hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder,
    };
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::ListParams, Api};
    use rustls::{
        crypto::aws_lc_rs::default_provider,
        pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
        ServerConfig,
    };
    use tokio::{net::TcpListener, sync::Notify};
    use tokio_rustls::TlsAcceptor;

    /// With this test we're trying to `assert` that our [`kube`] crate is (somewhat)
    /// version-synced with [`rustls`]. To give a friendlier error message on kube requests
    /// when there's a certificate problem, we must dig down into the [`kube::Error`].
    ///
    /// Certificate errors come under the [`kube::Error::Service`] variant, which holds
    /// a very annoying type-erased boxed error. Trying to `downcast` this is messy, so
    /// instead we check the debug message of the error.
    ///
    /// We want this error (or something similar) to happen:
    ///
    /// `Service(hyper_util::client::legacy::Error(Connect, Custom { kind: Other, error: Custom {
    /// kind: InvalidData, error: InvalidCertificate(Expired) } }))`.
    ///
    /// The part that we care about is the `InvalidCertificate`, the rest is not relevant.
    ///
    /// This test may fail if [`rustls`] changes the `error: InvalidCertificate` message,
    /// or any of the upper errors change.
    ///
    /// Relying on the `Debug` string version of the error, as the `Display` version is
    /// just a generic `ServiceError: client error (Connect)`.
    #[tokio::test]
    async fn kube_service_error_dependency_is_in_sync() {
        use kube::{Client, Config};

        let provider = default_provider();
        provider.install_default().unwrap();

        let notify = Arc::new(Notify::new());
        let wait_notify = notify.clone();

        tokio::spawn(async move {
            run_hyper_tls_server(notify).await;
        });

        // Wait until the server is listening before we start connecting.
        wait_notify.notified().await;

        let kube_config = Config::new("https://127.0.0.1:9669".parse().unwrap());
        let client = Client::try_from(kube_config).unwrap();

        // Manage pods
        let pods: Api<Pod> = Api::default_namespaced(client);

        let lp = ListParams::default().fields(&format!("legend={}", "hank"));

        let list = pods.list(&lp).await;
        assert!(
            list.as_ref()
                .is_err_and(|fail| { format!("{fail:?}").contains("InvalidCertificate") }),
            "We were expecting an error with `InvalidCertificate`, but got {list:?}!"
        );
    }

    /// Creates an [`hyper`] server with a broken certificate and a _catch-all_ route on
    /// `localhost:9669`.
    ///
    /// The certificate is generated here with [`rcgen::generate_simple_self_signed`].
    async fn run_hyper_tls_server(notify: Arc<Notify>) {
        use rcgen::generate_simple_self_signed;

        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 9669);

        let incoming = TcpListener::bind(&addr).await.unwrap();

        // Generate a certificate that should not work with kube.
        let cert_key = generate_simple_self_signed(vec!["mieszko.i".to_string()]).unwrap();
        let cert_pem = cert_key.cert.pem().into_bytes();
        let cert = CertificateDer::from_pem_slice(&cert_pem).unwrap();

        let key_pem = cert_key.key_pair.serialize_pem().into_bytes();
        let key = PrivateKeyDer::from_pem_slice(&key_pem).unwrap();

        // Build TLS configuration.
        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();

        server_config.alpn_protocols =
            vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"http/1.0".to_vec()];
        let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));

        /// Catch-all handler, we don't care about the response, any request is supposed
        /// to fail due to `InvalidCertificate`.
        async fn handle_any_route(
            _: Request<Incoming>,
        ) -> Result<Response<Full<Bytes>>, hyper::Error> {
            Ok(Response::new(Full::default()))
        }

        let service = service_fn(handle_any_route);

        // We're ready for the client side to start.
        notify.notify_waiters();
        let (tcp_stream, _remote_addr) = incoming.accept().await.unwrap();

        let tls_acceptor = tls_acceptor.clone();
        tokio::spawn(async move {
            let tls_stream = tls_acceptor.accept(tcp_stream).await.unwrap();

            Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls_stream), service)
                .await
                .unwrap();
        });
    }
}
