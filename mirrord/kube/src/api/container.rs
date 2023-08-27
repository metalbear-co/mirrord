use std::{collections::HashSet, sync::LazyLock};

use k8s_openapi::api::core::v1::ContainerStatus;
use kube::Client;
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use rand::{
    distributions::{Alphanumeric, DistString},
    Rng,
};

use crate::{
    api::{kubernetes::AgentKubernetesConnectInfo, runtime::RuntimeData},
    error::Result,
};

pub mod ephemeral;
pub mod job;
pub mod targetless;
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

pub trait ContainerUpdater {
    type Update;

    fn name(&self) -> &str;

    fn connection_port(&self) -> u16;

    fn runtime_data(&self) -> Option<&RuntimeData>;

    fn as_update(&self, agent: &AgentConfig) -> Result<Self::Update>;
}

pub trait ContainerVariant {
    type Update;

    fn as_update(&self, agent: &AgentConfig) -> Result<Self::Update>;
}

pub struct ContainerUpdateParams {
    pub name: String,
    pub connection_port: u16,
    pub variant: ContainerUpdateVariant,
}

impl ContainerUpdateParams {
    pub fn new(runtime_data: Option<RuntimeData>) -> Self {
        let agent_gid: u16 = rand::thread_rng().gen_range(3000..u16::MAX);
        let agent_port: u16 = rand::thread_rng().gen_range(30000..=65535);

        let agent_name = format!(
            "mirrord-agent-{}",
            Alphanumeric
                .sample_string(&mut rand::thread_rng(), 10)
                .to_lowercase()
        );

        match runtime_data {
            Some(runtime_data) => Self::target(agent_name, agent_port, agent_gid, runtime_data),
            None => Self::targetless(agent_name, agent_port),
        }
    }

    pub fn targetless(name: String, connection_port: u16) -> Self {
        ContainerUpdateParams {
            name,
            connection_port,
            variant: ContainerUpdateVariant::Targetless,
        }
    }

    pub fn target(name: String, connection_port: u16, gid: u16, runtime_data: RuntimeData) -> Self {
        ContainerUpdateParams {
            name,
            connection_port,
            variant: ContainerUpdateVariant::Target { gid, runtime_data },
        }
    }
}

pub enum ContainerUpdateVariant {
    Target { gid: u16, runtime_data: RuntimeData },
    Targetless,
}

pub trait ContainerApi {
    async fn create_agent<P>(
        &self,
        client: &Client,
        agent: &AgentConfig,
        progress: &P,
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
