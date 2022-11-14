use std::marker::PhantomData;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use k8s_openapi::{api::core::v1::Pod, NamespaceResourceScope};
use kube::{Api, Client, Config};
use mirrord_config::{agent::AgentConfig, target::TargetConfig};
use mirrord_progress::TaskProgress;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use rand::Rng;
use tokio::sync::mpsc;
use tracing::error;

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

    async fn create_connection(
        client: &Client,
        agent: &AgentConfig,
        agent_port: u16,
        pod_agent_name: String,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_api(client, agent.namespace.as_deref());
        let mut port_forwarder = pod_api.portforward(&pod_agent_name, &[agent_port]).await?;

        let mut codec = actix_codec::Framed::new(
            port_forwarder.take_stream(agent_port).unwrap(),
            ClientCodec::new(),
        );

        let (in_tx, mut in_rx) = mpsc::channel(1000);
        let (out_tx, out_rx) = mpsc::channel(1000);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(msg) = in_rx.recv() => {
                        if let Err(fail) = codec.send(msg).await {
                            error!("Error sending client message: {:#?}", fail);
                            break;
                        }
                    }
                    Some(daemon_message) = codec.next() => {
                        match daemon_message {
                            Ok(msg) => {
                                let _ = out_tx.send(msg).await;
                            }
                            Err(err) => {
                                error!("Error receiving daemon message: {:?}", err);
                                break;
                            }
                        }
                    }
                    else => {
                        error!("agent disconnected");

                        break;
                    }
                }
            }
        });

        Ok((in_tx, out_rx))
    }

    async fn create_agent(
        &self,
        progress: &TaskProgress,
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
        progress: &TaskProgress,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err> {
        let runtime_data = self
            .target
            .path.as_ref().ok_or_else(|| KubeApiError::InvalidTarget(
                "No target specified. Please set the `MIRRORD_IMPERSONATED_TARGET` environment variable.".to_owned(),
            ))?
            .runtime_data(&self.client, self.target.namespace.as_deref())
            .await?;

        let agent_port: u16 = rand::thread_rng().gen_range(30000..=65535);

        let pod_agent_name = C::create_agent(
            &self.client,
            &self.agent,
            runtime_data,
            agent_port,
            progress,
        )
        .await?;

        Self::create_connection(&self.client, &self.agent, agent_port, pod_agent_name).await
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

        let progress = TaskProgress::new("agent initializing...");

        println!("{:#?}", api.create_agent(&progress).await);

        Ok(())
    }
}
