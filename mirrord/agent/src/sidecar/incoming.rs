use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt, io,
    sync::Arc,
};

use tokio::sync::mpsc;
use tracing::trace;

use crate::incoming::{PortRedirector, Redirected};

/// Sender used by the sidecar bridge/router to inject bridged connections into the existing
/// incoming pipeline.
#[derive(Clone, Debug)]
pub(crate) struct BridgeIngressTx {
    tx: mpsc::Sender<Redirected>,
}

#[derive(Debug)]
pub struct BridgeRedirectorError(Box<dyn Error + Send + Sync + 'static>);

impl fmt::Display for BridgeRedirectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for BridgeRedirectorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl From<io::Error> for BridgeRedirectorError {
    fn from(value: io::Error) -> Self {
        Self(Box::new(value))
    }
}

impl From<BridgeRedirectorError> for Arc<dyn Error + Send + Sync + 'static> {
    fn from(value: BridgeRedirectorError) -> Self {
        value.0.into()
    }
}

impl BridgeIngressTx {
    pub(crate) async fn send(
        &self,
        conn: Redirected,
    ) -> Result<(), mpsc::error::SendError<Redirected>> {
        trace!(source = %conn.source(), destination = %conn.destination(), "queue bridged incoming connection");
        self.tx.send(conn).await
    }
}

/// [`PortRedirector`] implementation that receives redirected connections from the sidecar bridge
/// instead of from iptables.
#[derive(Debug)]
pub(crate) struct BridgeRedirector {
    rx: mpsc::Receiver<Redirected>,
    subscriptions: HashSet<u16>,
    pending: VecDeque<Redirected>,
}

impl BridgeRedirector {
    pub fn new() -> (Self, BridgeIngressTx) {
        let (tx, rx) = mpsc::channel(32);

        (
            Self {
                rx,
                subscriptions: HashSet::new(),
                pending: VecDeque::new(),
            },
            BridgeIngressTx { tx },
        )
    }

    fn take_pending_connection(&mut self) -> Option<Redirected> {
        let pending = self.pending.len();

        for _ in 0..pending {
            let Some(conn) = self.pending.pop_front() else {
                break;
            };

            if self.subscriptions.contains(&conn.destination().port()) {
                return Some(conn);
            }

            self.pending.push_back(conn);
        }

        None
    }
}

impl PortRedirector for BridgeRedirector {
    type Error = BridgeRedirectorError;

    async fn add_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        self.subscriptions.insert(from_port);
        Ok(())
    }

    async fn remove_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        self.subscriptions.remove(&from_port);
        Ok(())
    }

    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        self.subscriptions.clear();
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

                    return Err(BridgeRedirectorError::from(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "bridge ingress channel closed",
                    )));
                }
            }
        }
    }
}
