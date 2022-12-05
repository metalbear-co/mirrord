use std::{collections::HashMap, path::PathBuf};

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcpSteal, TcpData},
    ConnectionId, Port,
};
use tokio::{
    net::TcpListener,
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::log::warn;

use self::{ip_tables::SafeIpTables, worker::StealWorker};
use crate::{
    error::{AgentError, Result},
    runtime::set_namespace,
    util::{ClientID, IndexAllocator, Subscriptions},
};

pub(super) mod api;
pub(super) mod connection;
mod ip_tables;
mod orig_dst;
pub(super) mod worker;

/// Commands from the agent that are passed down to the stealer worker, through [`TcpStealerAPI`].
///
/// These are the operations that the agent receives from the layer to make the _steal_ feature
/// work.
#[derive(Debug)]
enum Command {
    /// Contains the channel that's used by the stealer worker to respond back to the agent
    /// (stealer -> agent -> layer).
    NewClient(Sender<DaemonTcp>),

    /// A layer wants to subscribe to this [`Port`].
    ///
    /// The agent starts stealing traffic on this [`Port`].
    PortSubscribe(Port),

    /// A layer wants to unsubscribe from this [`Port`].
    ///
    /// The agent stops stealing traffic from this [`Port`].
    PortUnsubscribe(Port),

    /// Part of the [`Drop`] implementation of [`TcpStealerAPI`].
    ///
    /// Closes a layer connection, and unsubscribe its ports.
    AgentClosed,

    /// A connection here is a pair of ([`ReadHalf`], [`WriteHalf`]) streams that are used to
    /// capture a remote connection (the connection we're stealing data from).
    ConnectionUnsubscribe(ConnectionId),

    /// There is new data in the direction going from the local process to the end-user (Going
    /// via the layer and the agent  local-process -> layer --> agent --> end-user).
    ///
    /// Agent forwards this data to the other side of original connection.
    ResponseData(TcpData),
}

/// Association between a client (identified by the `client_id`) and a [`Command`].
///
/// The (agent -> worker) channel uses this, instead of naked [`Command`]s when communicating.
#[derive(Debug)]
pub struct StealerCommand {
    /// Identifies which layer instance is sending the [`Command`].
    client_id: ClientID,

    /// The command message sent from (layer -> agent) to be handled by the stealer worker.
    command: Command,
}

#[tracing::instrument(level = "trace", skip(rx, tx))]
pub async fn steal_worker(
    rx: Receiver<LayerTcpSteal>,
    tx: Sender<DaemonTcp>,
    pid: Option<u64>,
) -> Result<()> {
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace)?;
    }
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let listen_port = listener.local_addr()?.port();

    StealWorker::new(tx, listen_port)?.start(rx, listener).await
}
