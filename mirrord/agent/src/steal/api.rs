use mirrord_protocol::tcp::{DaemonTcp, HttpResponse, LayerTcpSteal, TcpData};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::*;
use crate::{
    error::{AgentError, Result},
    util::ClientId,
    watched_task::TaskStatus,
};

/// Bridges the communication between the agent and the [`TcpConnectionStealer`] task.
/// There is an API instance for each connected layer ("client"). All API instances send commands
/// On the same stealer command channel, where the layer-independent stealer listens to them.
#[derive(Debug)]
pub(crate) struct TcpStealerApi {
    /// Identifies which layer instance is associated with this API.
    client_id: ClientId,

    /// Channel that allows the agent to communicate with the stealer task.
    ///
    /// The agent controls the stealer task through this.
    command_tx: Sender<StealerCommand>,

    /// Channel that receives [`DaemonTcp`] messages from the stealer worker thread.
    ///
    /// This is where we get the messages that should be passed back to agent or layer.
    daemon_rx: Receiver<DaemonTcp>,

    /// View on the stealer task's status.
    task_status: TaskStatus,
}

impl TcpStealerApi {
    /// Initializes a [`TcpStealerApi`] and sends a message to [`TcpConnectionStealer`] signaling
    /// that we have a new client.
    #[tracing::instrument(level = "trace")]
    pub(crate) async fn new(
        client_id: ClientId,
        command_tx: Sender<StealerCommand>,
        task_status: TaskStatus,
        channel_size: usize,
    ) -> Result<Self, AgentError> {
        let (daemon_tx, daemon_rx) = mpsc::channel(channel_size);

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
            task_status,
        })
    }

    /// Send `command` to stealer, with the client id of the client that is using this API instance.
    async fn send_command(&self, command: Command) -> Result<()> {
        self.command_tx
            .send(StealerCommand {
                client_id: self.client_id,
                command,
            })
            .await
            .map_err(AgentError::from)
            .map_err(|e| self.task_status.check().unwrap_or(e))
    }

    /// Send `command` synchronously to stealer with `try_send`, with the client id of the client
    /// that is using this API instance.
    fn try_send_command(&self, command: Command) -> Result<()> {
        self.command_tx
            .try_send(StealerCommand {
                client_id: self.client_id,
                command,
            })
            .map_err(AgentError::from)
            .map_err(|e| self.task_status.check().unwrap_or(e))
    }

    /// Helper function that passes the [`DaemonTcp`] messages we generated in the
    /// [`TcpConnectionStealer`] task, back to the agent.
    ///
    /// Called in the [`ClientConnectionHandler`].
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn recv(&mut self) -> Result<DaemonTcp> {
        self.daemon_rx.recv().await.ok_or_else(|| {
            self.task_status
                .check()
                .unwrap_or(AgentError::ReceiverClosed)
        })
    }

    /// Handles the conversion of [`LayerTcpSteal::PortSubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::PortSubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn port_subscribe(&mut self, port_steal: StealType) -> Result<(), AgentError> {
        self.send_command(Command::PortSubscribe(port_steal)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::PortUnsubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::PortUnsubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn port_unsubscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.send_command(Command::PortUnsubscribe(port)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::ConnectionUnsubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::ConnectionUnsubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn connection_unsubscribe(
        &mut self,
        connection_id: ConnectionId,
    ) -> Result<(), AgentError> {
        self.send_command(Command::ConnectionUnsubscribe(connection_id))
            .await
    }

    /// Handles the conversion of [`LayerTcpSteal::TcpData`], that is passed from the
    /// agent, to an internal stealer command [`Command::ResponseData`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn client_data(&mut self, tcp_data: TcpData) -> Result<(), AgentError> {
        self.send_command(Command::ResponseData(tcp_data)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::HttpResponse`], that is passed from the
    /// agent, to an internal stealer command [`Command::HttpResponse`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn http_response(&mut self, response: HttpResponse) -> Result<(), AgentError> {
        self.send_command(Command::HttpResponse(response)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::ClientClose`], that is passed from the
    /// agent, to an internal stealer command [`Command::ClientClose`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    ///
    /// Called by the [`Drop`] implementation of [`TcpStealerApi`].
    pub(crate) fn close_client(&mut self) -> Result<(), AgentError> {
        self.try_send_command(Command::ClientClose)
    }

    pub(crate) async fn handle_client_message(&mut self, message: LayerTcpSteal) -> Result<()> {
        match message {
            LayerTcpSteal::PortSubscribe(port_steal) => self.port_subscribe(port_steal).await,
            LayerTcpSteal::ConnectionUnsubscribe(connection_id) => {
                self.connection_unsubscribe(connection_id).await
            }
            LayerTcpSteal::PortUnsubscribe(port) => self.port_unsubscribe(port).await,
            LayerTcpSteal::Data(tcp_data) => self.client_data(tcp_data).await,
            LayerTcpSteal::HttpResponse(response) => self.http_response(response).await,
        }
    }
}

impl Drop for TcpStealerApi {
    fn drop(&mut self) {
        self.close_client()
            .expect("Failed while dropping TcpStealerApi!")
    }
}
