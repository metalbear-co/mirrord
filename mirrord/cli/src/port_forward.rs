use std::net::SocketAddr;

use tokio_stream::{wrappers::TcpListenerStream, StreamMap};

use crate::{connection::AgentConnection, CliError, PortMapping};

pub struct PortForwarder {
    agent_con: AgentConnection,
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    // rx_connections: some type,
    // tx_connections: some type,
}

impl PortForwarder {
    pub fn new(_agent_connection: AgentConnection, _mappings: Vec<PortMapping>) -> Self {
        // open tcp listener for local addrs
        // verify mappings - all local addrs unique, no port 0? ask aviram

        // enter main loop in run()
        todo!()
    }

    pub async fn run(&self) -> Result<(), CliError> {
        // when accepting conn spawn tokio task to serve conn
        // when we get first data need to talk 2 agent and make outg. conn then proxy data
        // ssh syntax req. -L to create mapping
        todo!()
    }
}
// tokio select to recv from agent, recv conn, read from open conn, proxy data
// assc listener sock add w/ destination, resolve hostname via agent, make outgoing conn w/ listener
// socket add <-> destination, parse mapping from args
// read from conn from user -> make outgoing conn (lazy)
// serve each tcp conn in seperate tokio task to avoid blocking
