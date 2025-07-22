use mirrord_config::LayerConfig;
use mirrord_kube::api::{
    kubernetes::{AgentKubernetesConnectInfo, KubernetesAPI},
    wrap_raw_connection,
};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

use crate::agent_conn::AgentConnectionError;

pub async fn create_connection(
    config: &LayerConfig,
    connect_info: AgentKubernetesConnectInfo,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), AgentConnectionError> {
    let k8s_api = KubernetesAPI::create(config)
        .await
        .map_err(AgentConnectionError::Kube)?;

    let stream = k8s_api
        .create_connection_portforward(connect_info.clone())
        .await
        .map_err(AgentConnectionError::Kube)?;

    Ok(wrap_raw_connection(stream))
}
