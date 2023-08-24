use std::{collections::HashSet, sync::LazyLock};

use k8s_openapi::api::core::v1::ContainerStatus;
use kube::Client;
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;

use crate::{
    api::{kubernetes::AgentKubernetesConnectInfo, runtime::RuntimeData},
    error::Result,
};

pub mod ephemeral;
pub mod job;
pub mod util;

pub static SKIP_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "istio-proxy",
        "istio-init",
        "linkerd-proxy",
        "linkerd-init",
        "vault-agent",
        "vault-agent-init",
    ])
});

pub trait ContainerApi {
    async fn create_agent<P>(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: Option<RuntimeData>,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync;
}

/// Choose container logic:
/// 1. Try to find based on given name
/// 2. Try to find first container in pod that isn't a mesh side car
/// 3. Take first container in pod
pub fn choose_container<'a>(
    container_name: &Option<String>,
    container_statuses: &'a [ContainerStatus],
) -> Option<&'a ContainerStatus> {
    if let Some(name) = container_name {
        container_statuses
            .iter()
            .find(|&status| &status.name == name)
    } else {
        // Choose any container that isn't part of the skip list
        container_statuses
            .iter()
            .find(|&status| !SKIP_NAMES.contains(status.name.as_str()))
            .or_else(|| container_statuses.first())
    }
}
