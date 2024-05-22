use bytes::Bytes;
use futures::TryFutureExt;
use http::{Request, Response};
use http_body_util::Empty;
use hyper::{body::Incoming, client::conn};
use hyper_util::rt::TokioIo;
use k8s_cri::v1::{runtime_service_client::RuntimeServiceClient, ContainerStatusRequest};
use serde::Deserialize;
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tracing::error;

use crate::{
    error::{AgentError, Result},
    runtime::{ContainerInfo, ContainerRuntime},
};

static CRIO_DEFAULT_SOCK_PATH: &str = "/host/run/crio/crio.sock";

#[derive(Debug, Clone)]
pub(crate) struct CriOContainer {
    pub container_id: String,
}

#[derive(Deserialize, Debug)]
struct ContainerStatus {
    pid: u64,
}

impl CriOContainer {
    pub fn from_id(container_id: String) -> Self {
        CriOContainer { container_id }
    }
}

impl ContainerRuntime for CriOContainer {
    async fn get_info(&self) -> Result<ContainerInfo> {
        let channel = Endpoint::try_from("http://localhost")?
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

        // Not sure if the `.get("pid")` logic works as on OpenShift
        // we observed that the `pid` exists in the `info` field which is JSON encoded.
        // for now we're adding a fallback
        let pid: u64 = match status.info.get("pid") {
            Some(val) => val.parse().map_err(|err| {
                AgentError::MissingContainerInfo(format!("Failed to parse pid: {err:?}"))
            })?,
            None => {
                let info_json = status.info.get("info").ok_or_else(|| {
                    AgentError::MissingContainerInfo("no info found in status".into())
                })?;
                let info: ContainerStatus = serde_json::from_str(info_json).map_err(|err| {
                    AgentError::MissingContainerInfo(format!("Failed to parse pid: {err:?}"))
                })?;
                info.pid
            }
        };

        Ok(ContainerInfo::new(pid, Default::default()))
    }
}
