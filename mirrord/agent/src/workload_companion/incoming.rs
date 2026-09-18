use std::{error::Error, fmt, io, sync::Arc};

use tokio::sync::mpsc;
use tracing::{debug, trace};

use crate::incoming::{PortRedirector, Redirected};

/// Sends accepted remote-layer handoffs into the generic incoming traffic pipeline.
#[derive(Clone, Debug)]
pub(super) struct IncomingConnectionSender {
    tx: mpsc::Sender<Redirected>,
}

impl IncomingConnectionSender {
    /// The failed connection is not recoverable by callers, so expose only channel closure.
    pub(super) async fn send(&self, connection: Redirected) -> Result<(), ()> {
        trace!(
            source = %connection.source(),
            destination = %connection.destination(),
            "queue bridged incoming connection"
        );
        self.tx
            .send(connection)
            .await
            .inspect_err(|error| debug!(?error, "failed to queue bridged incoming connection"))
            .map_err(|_| ())
    }
}

#[derive(Debug)]
pub(super) struct RemoteLayerPortRedirectorError(Box<dyn Error + Send + Sync + 'static>);

impl fmt::Display for RemoteLayerPortRedirectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for RemoteLayerPortRedirectorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl From<io::Error> for RemoteLayerPortRedirectorError {
    fn from(error: io::Error) -> Self {
        Self(Box::new(error))
    }
}

impl From<RemoteLayerPortRedirectorError> for Arc<dyn Error + Send + Sync + 'static> {
    fn from(error: RemoteLayerPortRedirectorError) -> Self {
        error.0.into()
    }
}

/// Components that connect the handoff server to the generic incoming redirector task.
pub(super) struct RemoteLayerIncoming {
    pub(super) redirector: RemoteLayerPortRedirector,
    pub(super) sender: IncomingConnectionSender,
}

impl RemoteLayerIncoming {
    pub(super) fn new() -> Self {
        let (tx, connections_rx) = mpsc::channel(32);

        Self {
            redirector: RemoteLayerPortRedirector { connections_rx },
            sender: IncomingConnectionSender { tx },
        }
    }
}

/// [`PortRedirector`] backed by connections handed off from an injected remote layer.
pub(super) struct RemoteLayerPortRedirector {
    connections_rx: mpsc::Receiver<Redirected>,
}

impl PortRedirector for RemoteLayerPortRedirector {
    type Error = RemoteLayerPortRedirectorError;

    /// Does not add external per-port state because the remote layer already hands off every
    /// accepted connection to the incoming pipeline.
    async fn add_redirection(&mut self, _from_port: u16) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Does not remove external per-port state because the remote layer never creates any.
    async fn remove_redirection(&mut self, _from_port: u16) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Does not clean up external resources because the handoff channel owns none.
    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Reports that every remote-layer connection is fed into the incoming pipeline before a port
    /// subscription is considered, allowing a later subscription to steal its subsequent requests.
    fn accepts_connections_without_subscription(&self) -> bool {
        true
    }

    async fn next_connection(&mut self) -> Result<Redirected, Self::Error> {
        self.connections_rx.recv().await.ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "remote ingress channel closed").into()
        })
    }
}

#[cfg(test)]
mod test {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::{
        Request, Response, Version, client::conn::http2, server::conn::http2 as server_http2,
        service::service_fn,
    };
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tokio::{
        net::{TcpListener, TcpStream},
        time::{Duration, timeout},
    };

    use super::RemoteLayerIncoming;
    use crate::incoming::{Redirected, RedirectorTask, RedirectorTaskConfig, StolenTraffic};

    #[tokio::test]
    async fn steals_request_on_http2_connection_accepted_before_subscription() {
        let RemoteLayerIncoming { redirector, sender } = RemoteLayerIncoming::new();
        let (task, mut steal_handle, _) = RedirectorTask::new(
            redirector,
            Default::default(),
            Default::default(),
            RedirectorTaskConfig::from_env(),
        );
        let task = tokio::spawn(task.run());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = listener.local_addr().unwrap();
        let ((stream, source), client) =
            tokio::try_join!(listener.accept(), TcpStream::connect(destination),).unwrap();
        sender
            .send(Redirected::new(stream, source, destination, None))
            .await
            .unwrap();

        let (mut client, connection) =
            http2::handshake(TokioExecutor::default(), TokioIo::new(client))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);

        let passthrough = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            server_http2::Builder::new(TokioExecutor::default())
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(|request| async move {
                        assert_eq!(request.uri().path(), "/before-subscription");
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                            b"passed through",
                        ))))
                    }),
                )
                .await
                .unwrap();
        });

        let response = client
            .send_request(
                Request::builder()
                    .uri("http://example.com/before-subscription")
                    .body(Empty::<Bytes>::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(b"passed through"),
        );

        steal_handle.steal(destination.port()).await.unwrap();

        let request_task = tokio::spawn(async move {
            client
                .send_request(
                    Request::builder()
                        .uri("http://example.com/after-subscription")
                        .body(Empty::<Bytes>::new())
                        .unwrap(),
                )
                .await
        });

        let StolenTraffic::Http(stolen_request) =
            timeout(Duration::from_secs(2), steal_handle.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        else {
            panic!("expected stolen HTTP traffic");
        };
        assert_eq!(stolen_request.parts().version, Version::HTTP_2);
        assert_eq!(stolen_request.parts().uri.path(), "/after-subscription");

        drop(stolen_request);
        request_task.abort();
        connection.abort();
        passthrough.abort();
        drop(steal_handle);
        task.await.unwrap().unwrap();
    }
}
