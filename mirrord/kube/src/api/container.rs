use std::{collections::HashSet, sync::LazyLock};

use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
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

const TELEPRESENCE_CONTAINER_NAME: &str = "traffic-agent";

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
        "queue-proxy", // Knative
        TELEPRESENCE_CONTAINER_NAME,
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
    pub pod_ips: Option<String>,
}

impl ContainerParams {
    pub fn new(tls_cert: Option<String>, pod_ips: Option<String>) -> ContainerParams {
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
            tls_cert,
            pod_ips,
        }
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

#[tracing::instrument(level = "trace", ret)]
pub fn check_mesh_vendor(pod: &Pod) -> Option<MeshVendor> {
    const ISTIO: [&str; 2] = ["istio-proxy", "istio-init"];
    const LINKERD: [&str; 2] = ["linkerd-proxy", "linkerd-init"];
    const KUMA: [&str; 2] = ["kuma-sidecar", "kuma-init"];

    if pod
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("ambient.istio.io/redirection"))
        .map(|annotation| annotation == "enabled")
        .unwrap_or_default()
    {
        return Some(MeshVendor::IstioAmbient);
    }

    let container_statuses = pod.status.as_ref()?.container_statuses.as_ref()?;

    container_statuses.iter().find_map(|status| {
        if ISTIO.contains(&status.name.as_str()) {
            Some(MeshVendor::Istio)
        } else if LINKERD.contains(&status.name.as_str()) {
            Some(MeshVendor::Linkerd)
        } else if KUMA.contains(&status.name.as_str()) {
            Some(MeshVendor::Kuma)
        } else {
            None
        }
    })
}

/// Choose container logic:
///
/// 1. Try to find based on given name
/// 2. Try to find first container in pod that isn't a mesh sidecar
/// 3. Take first container in pod
///
/// We also check if we're in a mesh based on `MESH_LIST`, returning whether we are or not.
#[tracing::instrument(level = "trace", ret)]
pub fn choose_container<'a>(
    container_name: Option<&str>,
    container_statuses: &'a [ContainerStatus],
) -> (Option<&'a ContainerStatus>, bool) {
    let mut picked_from_many = false;

    if container_statuses
        .iter()
        .any(|status| status.name == TELEPRESENCE_CONTAINER_NAME)
    {
        tracing::warn!("Telepresence container detected, stealing/mirroring might not work.");
    }
    let container = if let Some(name) = container_name {
        container_statuses
            .iter()
            .find(|&status| status.name == name)
    } else {
        let mut container_refs = container_statuses
            .iter()
            .filter(|&status| !SKIP_NAMES.contains(status.name.as_str()));
        // Choose first container that isn't part of the skip list
        let container = container_refs.next().or_else(|| {
            tracing::warn!(
                "Target has only containers with names that we would otherwise skip. Picking one."
            );
            picked_from_many = container_statuses.len() > 1;
            container_statuses.first()
        });
        picked_from_many = picked_from_many || container_refs.next().is_some();
        container
    };

    // container_counter is only incremented if there is no specified container name.
    (container, picked_from_many)
}
