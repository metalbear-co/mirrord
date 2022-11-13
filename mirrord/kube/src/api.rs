use std::marker::PhantomData;

use async_trait::async_trait;
use k8s_openapi::NamespaceResourceScope;
use kube::{Api, Client, Config};
use mirrord_config::{agent::AgentConfig, target::TargetConfig};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

use crate::{
    api::{
        container::{ContainerApi, EphemeralContainer, JobContainer},
        runtime::RuntimeDataProvider,
    },
    error::{KubeApiError, Result},
};

mod container;
#[cfg(feature = "env_guard")]
mod env_guard;
mod runtime;

pub fn get_k8s_api<K>(client: &Client, namespace: Option<&str>) -> Api<K>
where
    K: kube::Resource<Scope = NamespaceResourceScope>,
    <K as kube::Resource>::DynamicType: Default,
{
    if let Some(namespace) = namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    }
}

#[async_trait]
pub trait KubernetesAPI {
    type Err;

    async fn create_agent(
        &self,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>;
}

pub struct LocalApi<Container = JobContainer> {
    client: Client,
    agent: AgentConfig,
    target: TargetConfig,
    container: PhantomData<Container>,
}

impl LocalApi {
    pub async fn job(agent: AgentConfig, target: TargetConfig) -> Result<LocalApi<JobContainer>> {
        LocalApi::<JobContainer>::create(agent, target).await
    }

    pub async fn ephemeral(
        agent: AgentConfig,
        target: TargetConfig,
    ) -> Result<LocalApi<EphemeralContainer>> {
        LocalApi::<EphemeralContainer>::create(agent, target).await
    }
}

impl<C> LocalApi<C> {
    async fn create(agent: AgentConfig, target: TargetConfig) -> Result<Self> {
        #[cfg(feature = "env_guard")]
        let _guard = env_guard::EnvVarGuard::new();

        #[cfg_attr(not(feature = "env_guard"), allow(unused_mut))]
        let mut config = Config::infer().await?;

        #[cfg(feature = "env_guard")]
        _guard.prepare_config(&mut config);

        let client = Client::try_from(config).map_err(KubeApiError::from)?;

        Ok(LocalApi {
            client,
            agent,
            target,
            container: PhantomData::<C>,
        })
    }
}

#[async_trait]
impl<C> KubernetesAPI for LocalApi<C>
where
    C: ContainerApi + Send + Sync,
{
    type Err = KubeApiError;

    async fn create_agent(
        &self,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err> {
        let rt = self
            .target
            .path.as_ref().ok_or_else(|| KubeApiError::InvalidTarget(
                "No target specified. Please set the `MIRRORD_IMPERSONATED_TARGET` environment variable.".to_owned(),
            ))?
            .runtime_data(&self.client, self.target.namespace.as_deref())
            .await?;

        println!("{:#?}", rt);

        todo!()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use mirrord_config::{
        agent::AgentFileConfig,
        config::MirrordConfig,
        target::{Target, TargetFileConfig},
    };

    use super::*;

    #[tokio::test]
    async fn builder() -> Result<()> {
        let target = Target::from_str("deploy/py-serv-deployment").unwrap();

        let api = LocalApi::job(
            AgentFileConfig::default().generate_config().unwrap(),
            TargetFileConfig::Simple(Some(target))
                .generate_config()
                .unwrap(),
        )
        .await?;

        println!("{:#?}", api.create_agent().await);

        Ok(())
    }
}
