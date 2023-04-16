use bytes::Bytes;
use futures::TryFutureExt;
use http::Request;
use http_body_util::Empty;
use hyper::client::conn;
use k8s_cri::v1alpha2::{runtime_service_client::RuntimeServiceClient, ContainerStatusRequest};
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tracing::{debug, error};

use crate::{
    error::Result,
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
}

impl ContainerRuntime for CriOContainer {
    async fn get_info(&self) -> Result<ContainerInfo> {
        let channel = Endpoint::try_from("http://localhost")
            .unwrap()
            .connect_with_connector(service_fn(move |_: Uri| {
                UnixStream::connect(CRIO_DEFAULT_SOCK_PATH).inspect_err(|err| error!("{err:?}"))
            }))
            .await
            .unwrap();

        let mut client = RuntimeServiceClient::new(channel);

        let status = client
            .container_status(ContainerStatusRequest {
                container_id: self.container_id.clone(),
                verbose: true,
            })
            .await
            .unwrap()
            .into_inner();

        let pid: u64 = status
            .info
            .get("pid")
            .and_then(|val| val.parse().ok())
            .unwrap();

        Ok(ContainerInfo::new(pid, Default::default()))
    }

    async fn pause(&self) -> Result<()> {
        let stream = UnixStream::connect(CRIO_DEFAULT_SOCK_PATH).await?;
        let (mut request_sender, connection) = conn::http1::handshake(stream).await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("Error in connection: {}", e);
            }
        });

        let request = Request::builder()
            .method("GET")
            .uri(format!("http://localhost/pause/{}", self.container_id))
            .body(Empty::<Bytes>::new())?;

        let res = request_sender.send_request(request).await?;

        debug!("{res:#?}");

        Ok(())
    }

    async fn unpause(&self) -> Result<()> {
        let stream = UnixStream::connect(CRIO_DEFAULT_SOCK_PATH).await?;
        let (mut request_sender, connection) = conn::http1::handshake(stream).await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("Error in connection: {}", e);
            }
        });

        let request = Request::builder()
            .method("GET")
            .uri(format!("http://localhost/unpause/{}", self.container_id))
            .body(Empty::<Bytes>::new())?;

        let res = request_sender.send_request(request).await?;

        debug!("{res:#?}");

        Ok(())
    }
}
