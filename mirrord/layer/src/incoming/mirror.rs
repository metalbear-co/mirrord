use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
};

use bimap::BiMap;
use mirrord_protocol::{
    tcp::{HttpRequestFallback, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, ConnectionId, Port,
};
use tracing::{debug, error, info, trace, warn};

use crate::{
    error::{LayerError, Result},
    incoming::{Listen, TcpHandler},
};

/// Handles traffic mirroring
#[derive(Default)]
pub struct TcpMirrorHandler {
    ports: HashSet<Listen>,
    /// LocalPort:RemotePort mapping.
    port_mapping: BiMap<u16, u16>,
}

impl TcpMirrorHandler {
    pub fn new(port_mapping: BiMap<u16, u16>) -> Self {
        Self {
            port_mapping,
            ..Default::default()
        }
    }
}

impl TcpHandler for TcpMirrorHandler {
    fn handle_listen(&mut self, listen: Listen) -> Result<()> {
        self.apply_port_mapping(&mut listen);
        let request_port = listen.requested_port;

        if self.ports_mut().replace(listen).is_some() {
            // Since this struct identifies listening sockets by their port (#1558), we can only
            // forward the incoming traffic to one socket with that port.
            info!(
                "Received listen hook message for port {request_port} while already listening. \
                Might be on different address. Sending all incoming traffic to newest socket."
            );
            return Ok(());
        }

        tx.send(ClientMessage::Tcp(LayerTcp::PortSubscribe(request_port)))
            .await
            .map_err(From::from)
    }

    fn handle_close(&mut self, port: Port) -> std::result::Result<(), LayerError> {
        if self.ports_mut().remove(&port) {
            tx.send(ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)))
                .await
                .map_err(From::from)
        } else {
            // was not listening on closed socket, could be an outgoing socket.
            Ok(())
        }
    }
}
