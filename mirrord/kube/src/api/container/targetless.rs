use k8s_openapi::api::batch::v1::Job;
use kube::Client;
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;

use crate::{
    api::{
        container::{
            job::{create_job_agent, JobVariant},
            ContainerApi, ContainerParams, ContainerVariant,
        },
        kubernetes::AgentKubernetesConnectInfo,
    },
    error::Result,
};

pub struct Targetless<'c, V> {
    agent: &'c AgentConfig,
    client: &'c Client,
    params: &'c ContainerParams,
    variant: &'c V,
}

impl<'c, V> Targetless<'c, V>
where
    V: ContainerVariant<Update = Job>,
{
    pub fn new(
        client: &'c Client,
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        variant: &'c V,
    ) -> Self {
        Targetless {
            agent,
            client,
            params,
            variant,
        }
    }
}

impl<'c> ContainerApi<JobVariant<'c>> for Targetless<'c, JobVariant<'c>> {
    async fn create_agent<P>(&self, progress: &P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        create_job_agent::<P, JobVariant>(
            self.client,
            self.agent,
            self.params,
            self.variant,
            progress,
        )
        .await
    }
}
