use std::{collections::HashSet, sync::LazyLock};

use k8s_openapi::api::core::v1::ContainerStatus;
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use mirrord_protocol::MeshVendor;
use rand::{
    distributions::{Alphanumeric, DistString},
    Rng,
};

use crate::{api::kubernetes::AgentKubernetesConnectInfo, error::Result};

pub mod ephemeral;
pub mod job;
pub mod pod;
pub mod targeted;
pub mod targetless;
pub mod util;

pub static SKIP_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "kuma-sidecar",
        "kuma-init",
        "istio-proxy",
        "istio-init",
        "linkerd-proxy",
        "linkerd-init",
        "vault-agent",
        "vault-agent-init",
    ])
});

#[derive(Clone, Debug)]
pub struct ContainerParams {
    pub name: String,
    pub gid: u16,
    pub port: u16,
    /// Value for [`AGENT_OPERATOR_CERT_ENV`](mirrord_protocol::AGENT_OPERATOR_CERT_ENV) set in
    /// the agent container.
    pub tls_cert: Option<String>,
}

impl ContainerParams {
    pub fn new() -> ContainerParams {
        let port: u16 = rand::thread_rng().gen_range(30000..=65535);
        let gid: u16 = rand::thread_rng().gen_range(3000..u16::MAX);

        let name = format!(
            "mirrord-agent-{}",
            Alphanumeric
                .sample_string(&mut rand::thread_rng(), 10)
                .to_lowercase()
        );

        ContainerParams {
            name,
            gid,
            port,
            tls_cert: None,
        }
    }
}

impl Default for ContainerParams {
    fn default() -> Self {
        Self::new()
    }
}

pub trait ContainerVariant {
    type Update;

    fn agent_config(&self) -> &AgentConfig;

    fn params(&self) -> &ContainerParams;

    fn as_update(&self) -> Self::Update;
}

impl<T> ContainerVariant for Box<T>
where
    T: ContainerVariant + ?Sized,
{
    type Update = T::Update;

    fn agent_config(&self) -> &AgentConfig {
        T::agent_config(self)
    }

    fn params(&self) -> &ContainerParams {
        T::params(self)
    }

    fn as_update(&self) -> Self::Update {
        T::as_update(self)
    }
}

pub trait ContainerApi<V>
where
    V: ContainerVariant,
{
    #[allow(async_fn_in_trait)]
    async fn create_agent<P>(&self, progress: &mut P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync;
}

/// Choose container logic:
///
/// 1. Try to find based on given name
/// 2. Try to find first container in pod that isn't a mesh side car
/// 3. Take first container in pod
///
/// We also check if we're in a mesh based on `MESH_LIST`, returning whether we are or not.
#[tracing::instrument(level = "trace", ret)]
pub fn choose_container<'a>(
    container_name: Option<&str>,
    container_statuses: &'a [ContainerStatus],
) -> (Option<&'a ContainerStatus>, Option<MeshVendor>) {
    const ISTIO: [&str; 2] = ["istio-proxy", "istio-init"];
    const LINKERD: [&str; 2] = ["linkerd-proxy", "linkerd-init"];
    const KUMA: [&str; 2] = ["kuma-sidecar", "kuma-init"];

    let mesh = container_statuses.iter().find_map(|status| {
        if ISTIO.contains(&status.name.as_str()) {
            Some(MeshVendor::Istio)
        } else if LINKERD.contains(&status.name.as_str()) {
            Some(MeshVendor::Linkerd)
        } else if KUMA.contains(&status.name.as_str()) {
            Some(MeshVendor::Kuma)
        } else {
            None
        }
    });

    let container = if let Some(name) = container_name {
        container_statuses
            .iter()
            .find(|&status| status.name == name)
    } else {
        // Choose any container that isn't part of the skip list
        container_statuses
            .iter()
            .find(|&status| !SKIP_NAMES.contains(status.name.as_str()))
            .or_else(|| container_statuses.first())
    };

    (container, mesh)
}
