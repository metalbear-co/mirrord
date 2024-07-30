use std::{collections::HashMap, net::SocketAddr};

use mirrord_protocol::DaemonMessage;
use tokio::{
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener,
    },
    select,
};
use tokio_stream::{wrappers::TcpListenerStream, StreamExt, StreamMap};

use crate::{connection::AgentConnection, PortMapping};

pub struct PortForwarder {
    // communicates with the agent (only TCP supported)
    agent_connection: AgentConnection,
    // associates local ports with destination ports
    mappings: HashMap<SocketAddr, SocketAddr>,
    // accepts connections from the user app in the form of a stream
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    // the reading half of the stream received by a listener for receiving data to send to the
    rx_connections: StreamMap<SocketAddr, OwnedReadHalf>,
    // the writing half of the stream received by a listener for sending data back to the user app
    tx_connections: HashMap<SocketAddr, OwnedWriteHalf>,
}

impl PortForwarder {
    pub(crate) async fn new(
        agent_connection: AgentConnection,
        parsed_mappings: Vec<PortMapping>,
    ) -> Result<Self, PortForwardError> {
        // open tcp listener for local addrs
        let mut listeners = StreamMap::new();
        let mut mappings = HashMap::new();

        for mapping in parsed_mappings {
            if listeners.contains_key(&mapping.local) {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::SetupError);
            }
            let listener = TcpListener::bind(mapping.local).await;
            match listener {
                Ok(listener) => {
                    listeners.insert(mapping.local, TcpListenerStream::new(listener));
                    mappings.insert(mapping.local, mapping.remote);
                }
                Err(_error) => return Err(PortForwardError::TcpListenerError),
            }
        }

        Ok(Self {
            agent_connection,
            mappings,
            listeners,
            rx_connections: StreamMap::new(),
            tx_connections: HashMap::new(),
        })
    }

    pub(crate) async fn run(&mut self) -> Result<(), PortForwardError> {
        loop {
            select! {
                message = self.agent_connection.receiver.recv() => match message {
                    // message incoming from agent
                    Some(message) => {
                        match message {
                            DaemonMessage::Close(_) => todo!(),                         // kills intproxy, kills portfwd as well?
                            DaemonMessage::Tcp(_) => todo!(),                           // pass back to user app with tx_connections
                            DaemonMessage::TcpSteal(_) => todo!(),                      // pass back to user app with tx_connections
                            DaemonMessage::TcpOutgoing(_) => todo!(),                   // outgoing?
                            DaemonMessage::UdpOutgoing(_) => todo!(),                   // udp not supported, ignore? or send err?
                            DaemonMessage::LogMessage(_) => todo!(),                    // log given?
                            DaemonMessage::File(_) => todo!(),                          // ???
                            DaemonMessage::Pong => todo!(),                             // do nothing? or part of setup handshake?
                            DaemonMessage::GetEnvVarsResponse(_) => todo!(),            // ???
                            DaemonMessage::GetAddrInfoResponse(_) => todo!(),           // ???
                            DaemonMessage::PauseTarget(_) => todo!(),                   // pause depricated?
                            DaemonMessage::SwitchProtocolVersionResponse(_) => todo!(), // ???
                        }
                    },
                    None => {
                        tracing::trace!("None message recieved, connection ended with agent");
                        break Ok(());
                    },
                },

                // stream coming from the user app
                message = self.listeners.next() => match message {
                    Some((socket, Ok(stream))) => {
                        // split the stream and add to rx/ tx
                        let (read, write) = stream.into_split();
                        self.rx_connections.insert(socket, read);
                        self.tx_connections.insert(socket, write);
                        // get destination socket from mappings
                        let _destination = self.mappings.get(&socket);
                        // TODO: spawn tokio task to proxy data here
                        tokio::spawn(async move {
                            // when first data received: talk 2 agent and make outgoing connection, then proxy data
                            // TODO: ClientMessage Variant?
                            // self.agent_connection.sender.send(ClientMessage);
                        });
                        todo!()
                    },
                    Some((_socket, Err(_error))) => {
                        // error from TcpStream
                        // TODO: remove stream from rx, tx, send error or Close to agent?
                        // self.agent_connection.sender.send(ClientMessage);
                        todo!()
                    },
                    None => todo!(),
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum PortForwardError {
    SetupError,
    TcpListenerError,
}
