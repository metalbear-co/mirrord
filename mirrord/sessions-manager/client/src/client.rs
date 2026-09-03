mod agent;
mod intproxy;

use std::sync::Arc;

pub use agent::{AgentClient, AgentControlPlane};
pub use intproxy::{IntproxyClient, SessionsManagerConnectInfo};
use tokio_util::sync::CancellationToken;

use crate::{
    config::SessionsManagerConfig, credentials::CredentialProvider, data_plane::DataPlaneTransport,
};

/// Fields and builder methods shared by [`AgentClient`] and [`IntproxyClient`], factored out so
/// each client only carries its own role-specific fields (replica/instance identity vs.
/// session/target-replica identity) on top of this.
pub(super) struct ClientBuilder<T> {
    pub(super) config: SessionsManagerConfig,
    pub(super) credentials: Arc<dyn CredentialProvider>,
    pub(super) cancellation: CancellationToken,
    pub(super) transport: T,
}

impl<T: DataPlaneTransport> ClientBuilder<T> {
    pub(super) fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    pub(super) fn with_transport<U: DataPlaneTransport>(self, transport: U) -> ClientBuilder<U> {
        ClientBuilder {
            config: self.config,
            credentials: self.credentials,
            cancellation: self.cancellation,
            transport,
        }
    }
}
