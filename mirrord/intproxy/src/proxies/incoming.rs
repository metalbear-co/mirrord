use mirrord_protocol::tcp::DaemonTcp;

use crate::{error::Result, protocol::IncomingRequest};

#[derive(Default)]
pub struct IncomingProxy {}

impl IncomingProxy {
    pub async fn handle_layer_request(&mut self, _request: IncomingRequest) -> Result<()> {
        todo!()
    }

    pub async fn handle_agent_message(&mut self, _message: DaemonTcp) -> Result<()> {
        todo!()
    }
}
