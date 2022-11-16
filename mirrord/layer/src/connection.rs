use std::time::Duration;

use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{KubernetesAPI, LocalApi, RawConnection},
    error::KubeApiError,
};
use mirrord_progress::TaskProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
use tracing::log::info;

use crate::{graceful_exit, FAIL_STILL_STUCK};

const FAIL_CREATE_AGENT: &str = r#"
mirrord-layer failed while trying to establish connection with the agent pod!

- Suggestions:

>> Check that the agent pod was created with `$ kubectl get pods`, it should look something like
   "mirrord-agent-qjys2dg9xk-rgnhr        1/1     Running   0              7s".

>> Check that you're using the correct kubernetes credentials (and configuration).

>> Check your kubernetes context match where the agent should be spawned.
"#;

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

pub(crate) async fn connect(
    config: &LayerConfig,
) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
    if let Some(address) = &config.connect_tcp {
        let stream = TcpStream::connect(address)
            .await
            .unwrap_or_else(|_| panic!("Failed to connect to TCP socket {address:?}"));

        RawConnection
            .create_connection(stream)
            .await
            .unwrap_or_else(|err| handle_error(err, config))
    } else {
        let progress = TaskProgress::new("agent initializing...");

        let k8s_api = LocalApi::create(config.agent.clone(), config.target.clone())
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

                // Set env var for children to re-use.
                std::env::set_var("MIRRORD_CONNECT_AGENT", &pod_agent_name);
                std::env::set_var("MIRRORD_CONNECT_PORT", agent_port.to_string());
                // So children won't show progress as well as it might confuse users
                std::env::set_var(mirrord_progress::MIRRORD_PROGRESS_ENV, "off");
                (pod_agent_name, agent_port)
            }
        };

        k8s_api
            .create_connection(agent_ref)
            .await
            .unwrap_or_else(|err| handle_error(err, config))
    }
}
