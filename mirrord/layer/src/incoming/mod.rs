use mirrord_protocol::Port;

use crate::{error::HookError, socket::id::SocketId};

// pub mod mirror;
// pub mod steal;

#[derive(Debug, Clone)]
pub struct Listen {
    pub mirror_port: Port,
    pub requested_port: Port,
    pub ipv6: bool,
    pub id: SocketId,
}

pub trait TcpHandler {
    /// Handle the socket close.
    fn handle_close(&self, port: Port) -> Result<(), HookError>;

    /// Handle listen request.
    fn handle_listen(&self, listen: Listen) -> Result<(), HookError>;
}
