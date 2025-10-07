use mirrord_config::LayerConfig;
use mirrord_kube::api::kubernetes::{AgentKubernetesConnectInfo, KubernetesAPI, UnpinStream};
use mirrord_progress::NullProgress;
use mirrord_protocol::io::{AsyncIO, Client, Connection};

use crate::agent_conn::AgentConnectionError;

pub async fn create_connection(
    config: &LayerConfig,
    connect_info: AgentKubernetesConnectInfo,
) -> Result<Connection<Client>, AgentConnectionError> {
    let k8s_api = KubernetesAPI::create(config, &NullProgress {})
        .await
        .map_err(AgentConnectionError::Kube)?;

    let stream = k8s_api
        .create_connection_portforward(connect_info.clone())
        .await
        .map_err(AgentConnectionError::Kube)?;

    Ok(Connection::from_stream(convert(stream)).await?)
}

// If I don't do this stuff rustc complains about some cursed lifetime
// error in AgentConnection::restart.
fn convert(t: Box<dyn UnpinStream>) -> impl AsyncIO {
    t
}
