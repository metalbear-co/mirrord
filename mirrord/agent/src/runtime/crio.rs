use bytes::Bytes;
use futures::TryFutureExt;
use http::{Request, Response};
use http_body_util::{BodyExt, Empty};
use hyper::{body::Incoming, client::conn};
use k8s_cri::v1alpha2::{runtime_service_client::RuntimeServiceClient, ContainerStatusRequest};
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tracing::error;

use crate::{
    error::{AgentError, Result},
    runtime::{ContainerInfo, ContainerRuntime},
};

static CRIO_DEFAULT_SOCK_PATH: &str = "/host/run/crio/crio.sock";

#[derive(Debug)]
pub(crate) struct CriOContainer {
    pub container_id: String,
}

impl CriOContainer {
    pub fn from_id(container_id: String) -> Self {
        CriOContainer { container_id }
    }

    async fn request(path: String) -> Result<Response<Incoming>> {
        let stream = UnixStream::connect(CRIO_DEFAULT_SOCK_PATH).await?;
        let (mut request_sender, connection) = conn::http1::handshake(stream).await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("Error in connection: {}", e);
            }
        });

        let response = request_sender
            .send_request(
                Request::builder()
                    .method("GET")
                    .header("Host", "localhost")
                    .uri(format!("http://localhost/{}", path))
                    .body(Empty::<Bytes>::new())?,
            )
            .await?;

        if response.status().is_success() {
            Ok(response)
        } else {
            let status_code = response.status();
            let err_body = response.into_body().collect().await?;

            Err(AgentError::PauseRuntimeError(format!(
                "Request to {path} failed -> status: {status_code} | err_body: {err_body:?}"
            )))
        }
    }
}

impl ContainerRuntime for CriOContainer {
    async fn get_info(&self) -> Result<ContainerInfo> {
        let channel = Endpoint::try_from("http://localhost")
            .unwrap()
            .connect_with_connector(service_fn(move |_: Uri| {
                UnixStream::connect(CRIO_DEFAULT_SOCK_PATH).inspect_err(|err| error!("{err:?}"))
            }))
            .await?;

        let mut client = RuntimeServiceClient::new(channel);

        let status = client
            .container_status(ContainerStatusRequest {
                container_id: self.container_id.clone(),
                verbose: true,
            })
            .await?
            .into_inner();

        let pid: u64 = status
            .info
            .get("pid")
            .and_then(|val| val.parse().ok())
            .ok_or(AgentError::MissingContainerInfo)?;

        Ok(ContainerInfo::new(pid, Default::default()))
    }

    async fn pause(&self) -> Result<()> {
        Self::request(format!("/pause/{}", self.container_id)).await?;

        Ok(())
    }

    async fn unpause(&self) -> Result<()> {
        Self::request(format!("/unpause/{}", self.container_id)).await?;

        Ok(())
    }
}
