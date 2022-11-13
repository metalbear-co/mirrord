use std::fmt::{Display, Formatter};

use futures::{AsyncRead, AsyncWrite};

use crate::error::Result;

pub trait KubeApi {
    type Stream: AsyncWrite + AsyncRead + Unpin;

    fn as_stream(&self) -> Self::Stream;
}

pub(crate) enum ContainerRuntime {
    Docker,
    Containerd,
}

impl ContainerRuntime {
    pub(crate) fn mount_path(&self) -> &str {
        match self {
            ContainerRuntime::Docker => "/var/run/docker.sock",
            ContainerRuntime::Containerd => "/run/",
        }
    }
}

impl Display for ContainerRuntime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerRuntime::Docker => write!(f, "docker"),
            ContainerRuntime::Containerd => write!(f, "containerd"),
        }
    }
}

pub trait ContainerApi {
    fn create_agent() -> Result<String>;
}

pub struct JobContainer;

impl ContainerApi for JobContainer {
    fn create_agent() -> Result<String> {
        todo!()
    }
}

pub struct EphemeralContainer;

impl ContainerApi for EphemeralContainer {
    fn create_agent() -> Result<String> {
        todo!()
    }
}

pub struct KubeApi<Container: ContainerApi> {
    container: Container,
}

impl<C> KubeApi<C> {
    pub fn job() -> KubeApi<JobContainer> {
        todo!()
    }

    pub fn ephemeral() -> KubeApi<EphemeralContainer> {
        todo!()
    }
}
