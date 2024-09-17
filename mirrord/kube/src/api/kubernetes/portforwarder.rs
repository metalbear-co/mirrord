use std::{future::Future, time::Duration};

use bytes::BytesMut;
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_retry::{
    strategy::{jitter, ExponentialBackoff},
    Retry,
};

use crate::{
    api::kubernetes::{get_k8s_resource_api, AgentKubernetesConnectInfo, UnpinStream},
    error::{KubeApiError, Result},
};

type RetryStrategy = dyn Iterator<Item = Duration> + Send;

async fn create_portforward_streams(
    pod_api: &Api<Pod>,
    connect_info: &AgentKubernetesConnectInfo,
    retry_strategy: &mut RetryStrategy,
) -> Result<(
    Box<dyn UnpinStream>,
    Box<dyn Future<Output = Option<String>> + Unpin + Send>,
)> {
    let ports = &[connect_info.agent_port];
    let mut port_forwarder = Retry::spawn(retry_strategy, || {
        tracing::trace!("port-forward to pod {:?}", &connect_info);
        pod_api.portforward(&connect_info.pod_name, ports)
    })
    .await?;

    let stream = Box::new(
        port_forwarder
            .take_stream(connect_info.agent_port)
            .ok_or(KubeApiError::PortForwardFailed)?,
    );

    let error_future = Box::new(
        port_forwarder
            .take_error(connect_info.agent_port)
            .ok_or(KubeApiError::PortForwardFailed)?,
    );

    Ok((stream, error_future))
}

pub struct SinglePortForwarder {
    connect_info: AgentKubernetesConnectInfo,

    retry_strategy: Box<RetryStrategy>,

    pod_api: Api<Pod>,

    sink: Box<dyn UnpinStream>,

    stream: Box<dyn UnpinStream>,

    error_future: Box<dyn Future<Output = Option<String>> + Unpin + Send>,
}

impl SinglePortForwarder {
    pub async fn connect(
        client: &Client,
        connect_info: AgentKubernetesConnectInfo,
        sink: Box<dyn UnpinStream>,
    ) -> Result<Self> {
        let mut retry_strategy = Box::new(ExponentialBackoff::from_millis(10).map(jitter).take(5));

        let pod_api: Api<Pod> = get_k8s_resource_api(client, connect_info.namespace.as_deref());

        let (stream, error_future) =
            create_portforward_streams(&pod_api, &connect_info, &mut retry_strategy).await?;

        Ok(SinglePortForwarder {
            connect_info,
            retry_strategy,
            pod_api,
            sink,
            stream,
            error_future,
        })
    }

    pub async fn into_retry_future(self) {
        let SinglePortForwarder {
            mut error_future,
            mut stream,
            mut sink,
            retry_strategy,
            connect_info,
            pod_api,
        } = self;

        let mut retry_strategy = retry_strategy.peekable();

        let mut to_pod_buffer = BytesMut::with_capacity(1024);
        let mut from_pod_buffer = BytesMut::with_capacity(1024);

        loop {
            tokio::select! {
                error = error_future.as_mut() => {
                    tracing::error!("todo: {error:#?}");

                    if retry_strategy.peek().is_none() {
                        break;
                    }

                    match create_portforward_streams(&pod_api, &connect_info, &mut retry_strategy).await {
                        Ok((next_stream, next_error_future)) => {
                            stream = next_stream;
                            error_future = next_error_future;
                        }
                        Err(error) => {
                            tracing::error!("todo: {error:#?}");

                            break;
                        }
                    }
                }
                from_pod_read = stream.read_buf(&mut from_pod_buffer) => {
                    match from_pod_read {
                        Ok(amount) => {
                            #[allow(clippy::indexing_slicing)]
                            if let Err(error) = sink.write_all(&from_pod_buffer[..amount]).await {
                                tracing::error!("todo: {error}");

                                break;
                            }
                        }
                        Err(error) => {
                            tracing::error!("todo: {error}");

                            break;
                        }
                    }
                }
                to_pod_read = sink.read_buf(&mut to_pod_buffer) => {
                    match to_pod_read {
                        Ok(amount) => {
                            #[allow(clippy::indexing_slicing)]
                            if let Err(error) = stream.write_all(&to_pod_buffer[..amount]).await {
                                tracing::error!("todo: {error}");

                                break;
                            }
                        }
                        Err(error) => {
                            tracing::error!("todo: {error}");

                            break;
                        }
                    }
                }
            }
        }

        let _ = stream.shutdown().await;
        let _ = sink.shutdown().await;
    }
}

pub async fn retry_portforward(
    client: &Client,
    connect_info: AgentKubernetesConnectInfo,
) -> Result<(Box<dyn UnpinStream>, SinglePortForwarder)> {
    let (lhs, rhs) = tokio::io::duplex(64);

    let port_forwarder = SinglePortForwarder::connect(client, connect_info, Box::new(rhs)).await?;

    Ok((Box::new(lhs), port_forwarder))
}
