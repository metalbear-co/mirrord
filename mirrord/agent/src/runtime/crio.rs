use k8s_cri::v1::{runtime_service_client::RuntimeServiceClient, ContainerStatusRequest};
use serde::Deserialize;
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tracing::error;

use super::ContainerRuntimeError;
use crate::runtime::{error::ContainerRuntimeResult, ContainerInfo, ContainerRuntime};

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
    async fn get_info(&self) -> ContainerRuntimeResult<ContainerInfo> {
        let channel = Endpoint::try_from("http://localhost")
            .map_err(ContainerRuntimeError::crio)?
            .connect_with_connector(service_fn(move |_: Uri| async {
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(
                    UnixStream::connect(CRIO_DEFAULT_SOCK_PATH)
                        .await
                        .inspect_err(|err| error!("{err:?}"))?,
                ))
            }))
            .await
            .map_err(ContainerRuntimeError::crio)?;

        let mut client = RuntimeServiceClient::new(channel);

        let status = client
            .container_status(ContainerStatusRequest {
                container_id: self.container_id.clone(),
                verbose: true,
            })
            .await
            .map_err(ContainerRuntimeError::crio)?
            .into_inner();

        // Not sure if the `.get("pid")` logic works as on OpenShift
        // we observed that the `pid` exists in the `info` field which is JSON encoded.
        // for now we're adding a fallback
        let pid: u64 = match status.info.get("pid") {
            Some(val) => val.parse().map_err(|_| {
                ContainerRuntimeError::crio("failed to parse pid from the runtime response")
            })?,
            None => {
                let info_json = status.info.get("info").ok_or_else(|| {
                    ContainerRuntimeError::crio("info not found in the runtime response status")
                })?;
                let info: ContainerStatus =
                    serde_json::from_str(info_json).map_err(ContainerRuntimeError::crio)?;
                info.pid
            }
        };

        Ok(ContainerInfo::new(pid, Default::default()))
    }
}
