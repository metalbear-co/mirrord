use std::marker::PhantomData;

use actix_codec::{AsyncRead, AsyncWrite};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use k8s_openapi::{api::core::v1::Pod, NamespaceResourceScope};
use kube::{Api, Client, Config};
use mirrord_config::{agent::AgentConfig, target::TargetConfig};
use mirrord_progress::TaskProgress;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use rand::Rng;
use tokio::{net::TcpStream, sync::mpsc};
use tracing::{error, info};

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

fn wrap_raw_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::new());

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

#[async_trait]
pub trait KubernetesAPI {
    type AgentRef;
    type Err;

    async fn create_connection(
        &self,
        agent_ref: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>;

    async fn create_agent(&self, progress: &TaskProgress) -> Result<Self::AgentRef, Self::Err>;
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
    type AgentRef = (String, u16);
    type Err = KubeApiError;

    async fn create_connection(
        &self,
        (pod_agent_name, agent_port): Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_api(&self.client, self.agent.namespace.as_deref());
        let mut port_forwarder = pod_api.portforward(&pod_agent_name, &[agent_port]).await?;

        wrap_raw_connection(port_forwarder.take_stream(agent_port).unwrap())
    }

    async fn create_agent(&self, progress: &TaskProgress) -> Result<Self::AgentRef, Self::Err> {
        let runtime_data = self
            .target
            .path.as_ref().ok_or_else(|| KubeApiError::InvalidTarget(
                "No target specified. Please set the `MIRRORD_IMPERSONATED_TARGET` environment variable.".to_owned(),
            ))?
            .runtime_data(&self.client, self.target.namespace.as_deref())
            .await?;

        info!("No existing agent, spawning new one.");
        let agent_port: u16 = rand::thread_rng().gen_range(30000..=65535);
        info!("Using port `{agent_port:?}` for communication");

        let pod_agent_name = C::create_agent(
            &self.client,
            &self.agent,
            runtime_data,
            agent_port,
            progress,
        )
        .await?;

        Ok((pod_agent_name, agent_port))
    }
}

pub struct RawConnection;

#[async_trait]
impl KubernetesAPI for RawConnection {
    type AgentRef = TcpStream;
    type Err = KubeApiError;

    async fn create_connection(
        &self,
        stream: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        wrap_raw_connection(stream)
    }

    async fn create_agent(&self, _: &TaskProgress) -> Result<Self::AgentRef, Self::Err> {
        panic!("RawConnection cannot create agent");
    }
}
