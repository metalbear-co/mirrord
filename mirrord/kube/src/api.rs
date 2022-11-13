use mirrord_config::{agent::AgentConfig, target::Target};
use tokio::io::AsyncRead;

use crate::{
    api::container::{EphemeralContainer, JobContainer},
    error::Result,
};

mod container;
mod runtime;

pub trait KubeApi {
    fn create_agent() -> impl AsyncRead;
}

#[derive(Debug)]
pub struct LocalKubeApi<Container = JobContainer> {
    agent: AgentConfig,
    target: Target,
    container: Container,
}

impl LocalKubeApi {
    pub fn job(agent: AgentConfig, target: Target) -> LocalKubeApi<JobContainer> {
        LocalKubeApi {
            agent,
            target,
            container: JobContainer,
        }
    }

    pub fn ephemeral(agent: AgentConfig, target: Target) -> LocalKubeApi<EphemeralContainer> {
        LocalKubeApi {
            agent,
            target,
            container: EphemeralContainer,
        }
    }
}

#[cfg(test)]
mod tests {

    use std::str::FromStr;

    use mirrord_config::{agent::AgentFileConfig, config::MirrordConfig};

    use super::*;

    #[test]
    fn builder() -> Result<()> {
        let api = LocalKubeApi::job(
            AgentFileConfig::default().generate_config().unwrap(),
            Target::from_str("pod/foo/container/bar").unwrap(),
        );

        println!("{:#?}", api);

        Ok(())
    }
}
