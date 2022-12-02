use mirrord_protocol::tcp::DaemonTcp;
use tokio::sync::mpsc::{Receiver, Sender};

use super::*;
use crate::{
    error::{AgentError, Result},
    util::ClientID,
};

#[derive(Debug)]
pub(crate) struct TcpStealerApi {
    /// Identifies which layer instance is associated with this API.
    client_id: ClientID,

    /// Channel that allows the agent to communicate with the stealer task.
    ///
    /// The agent controls the stealer task through this.
    command_tx: Sender<StealerCommand>,

    /// Channel that receives [`DaemonTcp`] messages from the stealer worker thread.
    ///
    /// This is where we get the messages that should be passed back to agent or layer.
    daemon_rx: Receiver<DaemonTcp>,
}

impl TcpStealerApi {
    #[tracing::instrument(level = "debug")]
    pub(crate) async fn new(
        client_id: ClientID,
        command_tx: Sender<StealerCommand>,
        (daemon_tx, daemon_rx): (Sender<DaemonTcp>, Receiver<DaemonTcp>),
    ) -> Result<Self, AgentError> {
        command_tx
            .send(StealerCommand {
                client_id,
                command: Command::NewAgent(daemon_tx),
            })
            .await?;

        Ok(Self {
            client_id,
            command_tx,
            daemon_rx,
        })
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub(crate) async fn recv(&mut self) -> Option<DaemonTcp> {
        todo!()
    }

    pub(crate) async fn subscribe(&mut self, port: Port) -> Result<(), AgentError> {
        todo!()
    }

    pub(crate) async fn connection_unsubscribe(
        &mut self,
        connection_id: ConnectionId,
    ) -> Result<(), AgentError> {
        todo!()
    }

    pub(crate) async fn port_unsubscribe(&mut self, port: Port) -> Result<(), AgentError> {
        todo!()
    }
}
