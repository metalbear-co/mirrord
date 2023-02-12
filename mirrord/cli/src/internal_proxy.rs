//! Internal proxy is accepting connection from local layers and forward it to agent
//! while having 1:1 relationship - each layer connection is another agent connection.
//! This might be changed later on.
//! The main advantage of this design is that we remove kube logic from the layer itself,
//! thus eliminating bugs that happen due to mix of remote env vars in our code
//! (previously was solved using envguard which wasn't good enough)
//! The proxy will either directly connect to an existing agent (currently only used for tests),
//! or let the [`OperatorApi`] handle the connection.




use tokio::sync::mpsc::{Receiver, Sender};
use tracing::log::info;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use tokio::{net::TcpListener, time::timeout};

use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{kubernetes::KubernetesAPI, AgentManagment, Connection},
    error::KubeApiError,
};
use mirrord_operator::client::{OperatorApi, OperatorApiError};
use mirrord_progress::NoProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use crate::error::{InternalProxyError, Result};

const FIRST_CONNECTION_TIMEOUT: u64 = 5;

fn print_port(listener: &TcpListener) -> Result<()> {
    let port = listener
        .local_addr()
        .map_err(|e| InternalProxyError::LocalPortError(e))?
        .port();
    println!("{port:?}");
    Ok(())
}


async fn handle_connection(stream: TcpStream) {
    let mut stream = stream;
    let mut buf = [0; 1024];
    let n = stream.read(&mut buf).await?;
    let data = &buf[..n];
    let data = String::from_utf8_lossy(data);
    println!("Received: {:?}", data);
    Ok(())
}

pub(crate) async fn proxy() -> Result<()> {
    // Let it assign port for us then print it for the user.
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| InternalProxyError::ListenError(e))?;

    print_port(&listener)?;

    // wait for first connection `FIRST_CONNECTION_TIMEOUT` seconds, or timeout.
    let stream = timeout(
        Duration::from_secs(FIRST_CONNECTION_TIMEOUT),
        listener.accept(),
    )
    .await
    .map_err(|_| InternalProxyError::FirstConnectionTimeout)?
    .map_err(|e| InternalProxyError::AcceptError(e))?;

    loop {}
    Ok(())
}



use crate::{graceful_exit, FAIL_STILL_STUCK};

/// Helpful message that we print to the user in a [`graceful_exit`] call, when we get a
/// [`KubeApiError`], or an [`OperatorApiError].
const FAIL_CREATE_AGENT: &str = r#"
mirrord-layer failed while trying to establish connection with the agent pod!

- Suggestions:

>> Check that the agent pod was created with `$ kubectl get pods`, it should look something like
   "mirrord-agent-qjys2dg9xk-rgnhr        1/1     Running   0              7s".

>> Check that you're using the correct kubernetes credentials (and configuration).

>> Check your kubernetes' context matches where the agent should be spawned.
"#;

/// Handles a [`KubeApiError`] by printing what went wrong to the user, and closing the  program
/// with [`graceful_exit`].
///
/// Used in [`connect`], as we treat every error there as fatal.
fn handle_error(err: KubeApiError, config: &LayerConfig) -> ! {
    match err {
        KubeApiError::KubeError(kube::Error::HyperError(err)) => {
            eprintln!("\nmirrord encountered an error accessing the Kubernetes API.\n");
            if !config.accept_invalid_certificates {
                eprintln!("Consider passing --accept-invalid-certificates.\n");
            }

            match err.into_cause() {
                Some(cause) => graceful_exit!("Exiting due to {}", cause),
                None => graceful_exit!("mirrord got KubeError::HyperError"),
            }
        }
        _ => graceful_exit!("{FAIL_CREATE_AGENT}{FAIL_STILL_STUCK} with error {err}"),
    }
}

/// Calls [`graceful_exit`] when we have an [`OperatorApiError`].
///
/// Used in [`connect`] (same as [`handle_error`]), but only when `LayerConfig::operator` is
/// set to `true` and an operator is installed in the cluster.
fn handle_operator_error(err: OperatorApiError) -> ! {
    graceful_exit!("{FAIL_CREATE_AGENT}{FAIL_STILL_STUCK} with error {err}")
}

/// Initializes the agent pod with [`KubernetesAPI`], and connects to it, returning an API that the
/// layer uses to communicate with it, in the form of a [`Sender`] for [`ClientMessage`]s, and a
/// [`Receiver`] for [`DaemonMessage`]s.
///
/// ## Direct connection
///
/// There is support for connecting directly to an existing pod (instead of going through
/// `KubernetesAPI::create`) by setting `LayerConfig::connect_tcp`.
///
/// This is used for mirrord tests only.
///
/// ## Operator
///
/// The [`OperatorApi`] takes over the connection procedure if `LayerConfig::operator` is
/// set to `true` and an operator is installed in the cluster. We use `OperatorApi::discover`
/// instead of [`KubernetesAPI`].
pub(crate) async fn connect(
    config: &LayerConfig,
) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
    let progress = NoProgress;

    if let Some(address) = &config.connect_tcp {
        Connection(address)
            .connect(&progress)
            .await
            .unwrap_or_else(|err| handle_error(err, config))
    } else if config.operator && let Some(connection) = OperatorApi::discover(config).await.transpose() {
        connection.unwrap_or_else(|err| handle_operator_error(err))
    } else {
        let k8s_api = KubernetesAPI::create(config)
            .await
            .unwrap_or_else(|err| handle_error(err, config));

        let agent_ref = {
            if let (Some(pod_agent_name), Some(agent_port)) =
                (&config.connect_agent_name, config.connect_agent_port)
            {
                info!(
                    "Reusing existing agent {:?}, port {:?}",
                    pod_agent_name, agent_port
                );
                (pod_agent_name.to_owned(), agent_port)
            } else {
                let (pod_agent_name, agent_port) = tokio::time::timeout(
                    Duration::from_secs(config.agent.startup_timeout),
                    k8s_api.create_agent(&progress),
                )
                .await
                .unwrap_or(Err(KubeApiError::AgentReadyTimeout))
                .unwrap_or_else(|err| handle_error(err, config));
                info!("Created new agent {pod_agent_name}, port {agent_port}");

                // Set env var for children to re-use.
                std::env::set_var("MIRRORD_CONNECT_AGENT", &pod_agent_name);
                std::env::set_var("MIRRORD_CONNECT_PORT", agent_port.to_string());

                (pod_agent_name, agent_port)
            }
        };

        k8s_api
            .create_connection(agent_ref)
            .await
            .unwrap_or_else(|err| handle_error(err, config))
    }
}
