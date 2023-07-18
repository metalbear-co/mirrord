//! Module for the mirrord-layer/mirrord-agent connection mechanism.
//!
//! The layer will connect to the internal proxy to communicate with the agent.
use std::net::SocketAddr;

use mirrord_connection::wrap_raw_connection;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
use tracing::error;

use crate::graceful_exit;

const FAIL_STILL_STUCK: &str = r#"
- If you're still stuck:

>> Please open a new bug report at https://github.com/metalbear-co/mirrord/issues/new/choose

>> Or join our Discord https://discord.gg/metalbear and request help in #mirrord-help

>> Or email us at hi@metalbear.co

"#;

/// Connects to the internal proxy in given `SocketAddr`
/// layer uses to communicate with it, in the form of a [`Sender`] for [`ClientMessage`]s, and a
/// [`Receiver`] for [`DaemonMessage`]s.
pub(crate) async fn connect_to_proxy(
    addr: SocketAddr,
) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
    let stream = match TcpStream::connect(addr).await {
        Ok(stream) => stream,
        Err(e) => {
            error!("Couldn't connect to internal proxy: {e:?}, {addr:?}");
            graceful_exit!("{FAIL_STILL_STUCK:?}");
        }
    };
    wrap_raw_connection(stream)
}
