use mirrord_kube::api::kubernetes::AgentKubernetesConnectInfo;
use mirrord_operator::client::OperatorSessionInformation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentConnectInfo {
    /// Connect to the agent through the operator.
    Operator(OperatorSessionInformation),
    /// Connect directly to the agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
}
