use mirrord_protocol::tcp::{DaemonTcp, TcpData};
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
                command: Command::NewClient(daemon_tx),
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

    /// Handles the conversion of [`LayerTcpSteal::PortSubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::PortSubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn port_subscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.command_tx
            .send(StealerCommand {
                client_id: self.client_id,
                command: Command::PortSubscribe(port),
            })
            .await
            .map_err(From::from)
    }

    /// Handles the conversion of [`LayerTcpSteal::PortUnsubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::PortUnsubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn port_unsubscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.command_tx
            .send(StealerCommand {
                client_id: self.client_id,
                command: Command::PortUnsubscribe(port),
            })
            .await
            .map_err(From::from)
    }

    pub(crate) async fn connection_unsubscribe(
        &mut self,
        connection_id: ConnectionId,
    ) -> Result<(), AgentError> {
        self.command_tx
            .send(StealerCommand {
                client_id: self.client_id,
                command: Command::ConnectionUnsubscribe(connection_id),
            })
            .await
            .map_err(From::from)
    }

    pub(crate) async fn client_data(&mut self, tcp_data: TcpData) -> Result<(), AgentError> {
        todo!()
    }
}
