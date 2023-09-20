use kube::Client;
use mirrord_progress::Progress;
use tracing::debug;

use crate::{
    api::{
        container::{
            ephemeral::{create_ephemeral_agent, EphemeralTargetedVariant},
            job::{create_job_agent, JobTargetedVariant},
            ContainerApi, ContainerVariant,
        },
        kubernetes::AgentKubernetesConnectInfo,
        runtime::{NodeCheck, RuntimeData},
    },
    error::{KubeApiError, Result},
};

pub struct Targeted<'c, V> {
    client: &'c Client,
    runtime_data: &'c RuntimeData,
    variant: &'c V,
}

impl<'c, V> Targeted<'c, V> {
    pub fn new(client: &'c Client, runtime_data: &'c RuntimeData, variant: &'c V) -> Self {
        Targeted {
            client,
            runtime_data,
            variant,
        }
    }
}

impl<'c> ContainerApi<JobTargetedVariant<'c>> for Targeted<'c, JobTargetedVariant<'c>> {
    async fn create_agent<P>(&self, progress: &mut P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        let Targeted {
            client,
            runtime_data,
            variant,
        } = self;

        if variant.agent_config().check_out_of_pods {
            match runtime_data.check_node(client).await {
                NodeCheck::Success => debug!("node is allocatable"),
                NodeCheck::Error(err) => {
                    debug!("unable to check if node is allocatable, {err}");
                }
                NodeCheck::Failed(node_name, pods) => {
                    progress.failure(Some("node is not allocatable"));

                    return Err(KubeApiError::NodePodLimitExceeded(node_name, pods));
                }
            }
        }

        create_job_agent::<P, JobTargetedVariant<'c>>(client, variant, progress).await
    }
}

impl<'c> ContainerApi<EphemeralTargetedVariant<'c>> for Targeted<'c, EphemeralTargetedVariant<'c>> {
    async fn create_agent<P>(&self, progress: &mut P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        let Targeted {
            client,
            runtime_data,
            variant,
        } = self;

        create_ephemeral_agent::<P, EphemeralTargetedVariant<'c>>(
            client,
            runtime_data,
            variant,
            progress,
        )
        .await
    }
}
