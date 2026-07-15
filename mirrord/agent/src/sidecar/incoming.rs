use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt, io,
    sync::{Arc, Mutex},
};

use mirrord_remote_layer_protocol::RemoteLayerSubscriptionsView;
use tokio::sync::mpsc;
use tracing::trace;

use crate::incoming::{PortRedirector, Redirected};

/// Sender used by the sidecar bridge/router to inject bridged connections into the existing
/// incoming pipeline.
#[derive(Clone, Debug)]
pub(crate) struct IncomingConnectionSender {
    tx: mpsc::Sender<Redirected>,
}

impl IncomingConnectionSender {
    pub(crate) async fn send(
        &self,
        conn: Redirected,
    ) -> Result<(), mpsc::error::SendError<Redirected>> {
        trace!(source = %conn.source(), destination = %conn.destination(), "queue bridged incoming connection");
        self.tx.send(conn).await
    }
}

#[derive(Debug)]
pub struct RemoteLayerPortRedirectorError(Box<dyn Error + Send + Sync + 'static>);

impl fmt::Display for RemoteLayerPortRedirectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for RemoteLayerPortRedirectorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl From<io::Error> for RemoteLayerPortRedirectorError {
    fn from(value: io::Error) -> Self {
        Self(Box::new(value))
    }
}

impl From<RemoteLayerPortRedirectorError> for Arc<dyn Error + Send + Sync + 'static> {
    fn from(value: RemoteLayerPortRedirectorError) -> Self {
        value.0.into()
    }
}

/// [`PortRedirector`] implementation that receives redirected connections from the sidecar bridge
/// instead of from iptables.
#[derive(Debug)]
pub(crate) struct RemoteLayerPortRedirector {
    rx: mpsc::Receiver<Redirected>,
    subscriptions: Arc<Mutex<HashSet<u16>>>,
    pending: VecDeque<Redirected>,
}

impl RemoteLayerPortRedirector {
    pub fn new() -> (Self, IncomingConnectionSender, RemoteLayerSubscriptionsView) {
        let (tx, rx) = mpsc::channel(32);
        let subscriptions = Arc::new(Mutex::new(HashSet::new()));

        (
            Self {
                rx,
                subscriptions: Arc::clone(&subscriptions),
                pending: VecDeque::new(),
            },
            IncomingConnectionSender { tx },
            RemoteLayerSubscriptionsView::new(subscriptions),
        )
    }

    fn take_pending_connection(&mut self) -> Option<Redirected> {
        let pending = self.pending.len();

        for _ in 0..pending {
            let Some(conn) = self.pending.pop_front() else {
                break;
            };

            let subscriptions = self
                .subscriptions
                .lock()
                .expect("remote-layer subscription lock failed");
            if subscriptions.contains(&conn.destination().port()) {
                return Some(conn);
            }

            drop(subscriptions);
            self.pending.push_back(conn);
        }

        None
    }
}

impl PortRedirector for RemoteLayerPortRedirector {
    type Error = RemoteLayerPortRedirectorError;

    async fn add_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        self.subscriptions
            .lock()
            .expect("remote-layer subscription lock failed")
            .insert(from_port);
        Ok(())
    }

    async fn remove_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        self.subscriptions
            .lock()
            .expect("remote-layer subscription lock failed")
            .remove(&from_port);
        Ok(())
    }

    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        self.subscriptions
            .lock()
            .expect("remote-layer subscription lock failed")
            .clear();
        self.pending.clear();
        Ok(())
    }

    async fn next_connection(&mut self) -> Result<Redirected, Self::Error> {
        loop {
            if let Some(conn) = self.take_pending_connection() {
                return Ok(conn);
            }

            match self.rx.recv().await {
                Some(conn) => self.pending.push_back(conn),
                None => {
                    if let Some(conn) = self.take_pending_connection() {
                        return Ok(conn);
                    }

                    return Err(RemoteLayerPortRedirectorError::from(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "bridge ingress channel closed",
                    )));
                }
            }
        }
    }
}
