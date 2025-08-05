use mirrord_protocol::{LogMessage, Port};
use tokio::sync::mpsc::Sender;

use crate::{
    http::filter::HttpFilter,
    incoming::{StolenHttp, StolenTcp},
    util::{ClientId, protocol_version::ClientProtocolVersion},
};

mod api;
mod subscriptions;
mod task;
#[cfg(test)]
mod test;

pub use api::TcpStealerApi;
pub use task::TcpStealerTask;

/// Commands from the agent that are passed down to the stealer worker, through [`TcpStealerApi`].
///
/// These are the operations that the agent receives from the layer to make the _steal_ feature
/// work.
#[derive(Debug)]
enum Command {
    /// Contains a channel that will be used by the [`TcpStealerTask`] to send messages to
    /// [`TcpStealerApi`].
    NewClient(Sender<StealerMessage>, ClientProtocolVersion),

    /// The layer wants to subscribe to this [`Port`].
    ///
    /// The agent starts stealing traffic from this [`Port`].
    PortSubscribe(Port, Option<HttpFilter>),

    /// The layer wants to unsubscribe from this [`Port`].
    ///
    /// The agent stops stealing traffic from this [`Port`].
    PortUnsubscribe(Port),
}

/// Sent from [`TcpStealerApi`]s to the [`TcpStealerTask`].`
#[derive(Debug)]
pub struct StealerCommand {
    /// Identifies which layer instance is sending the [`Command`].
    client_id: ClientId,
    /// The actual command for the task.
    command: Command,
}

/// Sent from the [`TcpStealerTask`] to the [`TcpStealerApi`]s.
enum StealerMessage {
    StolenTcp(StolenTcp),
    StolenHttp(StolenHttp),
    Log(LogMessage),
    PortSubscribed(Port),
}
