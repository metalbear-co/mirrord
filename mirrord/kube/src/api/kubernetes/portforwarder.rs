use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_retry::{
    Retry,
    strategy::{ExponentialBackoff, jitter},
};

use crate::{
    api::kubernetes::{AgentKubernetesConnectInfo, UnpinStream},
    error::{KubeApiError, Result},
};

/// A wrapper for [`AsyncRead`] & [`AsyncWrite`] that dosn't call shutdown from [`AsyncWrite`] api
/// but it is done manually with `manual_shutdown`
struct ManualShutdown<S> {
    inner: S,
}

impl<S> ManualShutdown<S> {
    fn new(inner: S) -> Self {
        ManualShutdown { inner }
    }
}

impl<S> ManualShutdown<S>
where
    S: AsyncWrite + Unpin,
{
    async fn manual_shutdown(&mut self) -> Result<(), std::io::Error> {
        self.inner.shutdown().await
    }
}

impl<S> AsyncRead for ManualShutdown<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for ManualShutdown<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    /// Does nothing, call `manual_shutdown` for actuall inner shutdown call
    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }
}

type RetryStrategy = dyn Iterator<Item = Duration> + Send;

pub async fn create_portforward_streams(
    pod_api: &Api<Pod>,
    connect_info: &AgentKubernetesConnectInfo,
    retry_strategy: &mut RetryStrategy,
) -> Result<(
    Box<dyn UnpinStream>,
    Box<dyn Future<Output = Option<String>> + Unpin + Send>,
)> {
    let ports = &[connect_info.agent_port];
    let mut port_forwarder = Retry::start(retry_strategy, || {
        tracing::trace!("port-forward to pod {:?}", &connect_info);
        pod_api.portforward(&connect_info.pod_name, ports)
    })
    .await
    .map_err(|error| KubeApiError::AgentPortForward(Box::new(error)))?;

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

    sink: ManualShutdown<Box<dyn UnpinStream>>,

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

        let pod_api: Api<Pod> = Api::namespaced(client.clone(), &connect_info.pod_namespace);

        let (stream, error_future) =
            create_portforward_streams(&pod_api, &connect_info, &mut retry_strategy).await?;

        let sink = ManualShutdown::new(sink);

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

        loop {
            tokio::select! {
                biased;

                error = error_future.as_mut() => {
                    if let Some(error) = error {
                        tracing::warn!(?connect_info, %error, "error while performing port-forward");
                    } else {
                        tracing::warn!(?connect_info, "port-forward connection closed without any error published in relevant channel");

                        break;
                    }

                    if retry_strategy.peek().is_none() {
                        tracing::warn!(?connect_info, "port-forward connection retry strategy has reached it's limit on attempts to connect, not attempting again.");

                        break;
                    }

                    match create_portforward_streams(&pod_api, &connect_info, &mut retry_strategy).await {
                        Ok((next_stream, next_error_future)) => {
                            let _ = stream.shutdown().await;

                            stream = next_stream;
                            error_future = next_error_future;

                            tracing::trace!(?connect_info, "retry connect successful");
                        }
                        Err(error) => {
                            tracing::error!(?connect_info, %error, "retry connect failed");

                            break;
                        }
                    }
                }
                copy_result = tokio::io::copy_bidirectional(&mut stream, &mut sink) => {
                    if let Err(error) = copy_result {
                        tracing::error!(?connect_info, %error, "unable to copy_bidirectional agent stream to local sink");
                    } else {
                        tracing::info!(?connect_info, "closed copy_bidirectional agent stream to local sink");
                    }

                    break;
                }
            }
        }

        let _ = sink.manual_shutdown().await;
    }
}

pub async fn retry_portforward(
    client: &Client,
    connect_info: AgentKubernetesConnectInfo,
) -> Result<(Box<dyn UnpinStream>, SinglePortForwarder)> {
    // use 1024 * 1024 to be identical to [`kube` implementation](https://github.com/kube-rs/kube/blob/ecbdafc214538aadc78ec8447f2fa12d0057492b/kube-client/src/api/portforward.rs#L101).
    let (lhs, rhs) = tokio::io::duplex(1024 * 1024);

    let port_forwarder = SinglePortForwarder::connect(client, connect_info, Box::new(rhs)).await?;

    Ok((Box::new(lhs), port_forwarder))
}

#[cfg(test)]
mod test {
    use http::{Request, Response, StatusCode};
    use kube::client::Body;

    use super::*;

    /// A proxy between mirrord and the API server can reject the port-forward upgrade with a
    /// response that is not a Kubernetes `Status`. The HTTP status and the response body are the
    /// only hints about what rejected it, so the error must keep both.
    #[tokio::test]
    async fn rejected_upgrade_keeps_status_and_body() {
        let (service, mut handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let pod_api = Api::<Pod>::namespaced(Client::new(service, "default"), "default");
        let connect_info = AgentKubernetesConnectInfo {
            pod_name: "mirrord-agent".to_owned(),
            pod_namespace: "default".to_owned(),
            agent_port: 44128,
        };

        let mut no_retries = std::iter::empty();

        let (result, ()) = tokio::join!(
            create_portforward_streams(&pod_api, &connect_info, &mut no_retries),
            async {
                let (_, send) = handle.next_request().await.unwrap();
                send.send_response(
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from(b"proxy rejected the upgrade".to_vec()))
                        .unwrap(),
                );
            },
        );

        let Err(error) = result else {
            panic!("port-forward must fail when the upgrade is rejected");
        };
        let message = error.to_string();
        assert!(
            message.starts_with("Agent port-forward WebSocket upgrade failed"),
            "{message}"
        );
        assert!(message.contains("500"), "{message}");
        assert!(message.contains("proxy rejected the upgrade"), "{message}");
    }
}
