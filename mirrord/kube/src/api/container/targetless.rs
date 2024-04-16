use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
use kube::Client;
use mirrord_progress::Progress;

use crate::{
    api::{
        container::{
            job::{create_job_agent, JobVariant},
            ContainerApi, ContainerVariant,
        },
        kubernetes::AgentKubernetesConnectInfo,
    },
    error::Result,
};

pub struct Targetless<'c, V> {
    client: &'c Client,
    variant: &'c V,
}

impl<'c, V> Targetless<'c, V>
where
    V: ContainerVariant<Update = Job>,
{
    pub fn new(client: &'c Client, variant: &'c V) -> Self {
        Targetless { client, variant }
    }
}

impl<'c, T> ContainerApi<JobVariant<T>> for Targetless<'c, JobVariant<T>>
where
    T: ContainerVariant<Update = Pod>,
{
    async fn create_agent<P>(&self, progress: &mut P) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        create_job_agent::<P, JobVariant<T>>(self.client, self.variant, progress).await
    }
}
