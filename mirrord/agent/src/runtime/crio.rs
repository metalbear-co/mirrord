use async_trait::async_trait;
use k8s_cri::v1alpha2::{runtime_service_client::RuntimeServiceClient, ContainerStatusRequest};
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tracing::debug;

use crate::{
    error::Result,
    runtime::{ContainerInfo, ContainerRuntime},
};

static CRIO_DEFAULT_SOCK_PATH: &str = "/host/var/run/crio/crio.sock";

#[derive(Debug)]
pub(crate) struct CriOContainer {
    pub container_id: String,
}

#[async_trait]
impl ContainerRuntime for CriOContainer {
    async fn get_info(&self) -> Result<ContainerInfo> {
        let channel = Endpoint::try_from("http://[::]")
            .unwrap()
            .connect_with_connector(service_fn(move |_: Uri| {
                UnixStream::connect(CRIO_DEFAULT_SOCK_PATH)
            }))
            .await
            .unwrap();

        let mut client = RuntimeServiceClient::new(channel);

        debug!(
            "{:#?}",
            client
                .container_status(ContainerStatusRequest {
                    container_id: self.container_id.clone(),
                    verbose: true
                })
                .await
        );

        todo!()
    }

    async fn pause(&self) -> Result<()> {
        todo!()
    }

    async fn unpause(&self) -> Result<()> {
        todo!()
    }
}
