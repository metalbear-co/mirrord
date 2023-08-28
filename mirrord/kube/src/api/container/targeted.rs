use kube::Client;
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use tracing::debug;

use crate::{
    api::{
        container::{
            ephemeral::{create_ephemeral_agent, EphemeralTargetedVariant},
            job::{create_job_agent, JobTargetedVariant},
            ContainerApi, ContainerParams,
        },
        kubernetes::AgentKubernetesConnectInfo,
        runtime::{NodeCheck, RuntimeData},
    },
    error::{KubeApiError, Result},
};

pub struct Targeted<'c, V> {
    agent: &'c AgentConfig,
    client: &'c Client,
    params: &'c ContainerParams,
    runtime_data: &'c RuntimeData,
    variant: &'c V,
}

impl<'c, V> Targeted<'c, V> {
    pub fn new(
        client: &'c Client,
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        runtime_data: &'c RuntimeData,
        variant: &'c V,
    ) -> Self {
        Targeted {
            agent,
            client,
            params,
            runtime_data,
            variant,
        }
    }
}

impl<'c> ContainerApi<JobTargetedVariant<'c>> for Targeted<'c, JobTargetedVariant<'c>> {
    async fn create_agent<P>(&self, progress: &P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        let Targeted {
            agent,
            client,
            params,
            runtime_data,
            variant,
        } = self;

        if agent.check_out_of_pods {
            let mut check_node = progress.subtask("checking if node is allocatable...");
            match runtime_data.check_node(client).await {
                NodeCheck::Success => check_node.success(Some("node is allocatable")),
                NodeCheck::Error(err) => {
                    debug!("{err}");
                    check_node.warning("unable to check if node is allocatable");
                }
                NodeCheck::Failed(node_name, pods) => {
                    check_node.failure(Some("node is not allocatable"));

                    return Err(KubeApiError::NodePodLimitExceeded(node_name, pods));
                }
            }
        }

        create_job_agent::<P, JobTargetedVariant<'c>>(client, agent, params, variant, progress)
            .await
    }
}

impl<'c> ContainerApi<EphemeralTargetedVariant<'c>> for Targeted<'c, EphemeralTargetedVariant<'c>> {
    async fn create_agent<P>(&self, progress: &P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        let Targeted {
            agent,
            client,
            params,
            runtime_data,
            variant,
        } = self;

        create_ephemeral_agent::<P, EphemeralTargetedVariant<'c>>(
            client,
            agent,
            params,
            runtime_data,
            variant,
            progress,
        )
        .await
    }
}
