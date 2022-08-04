use std::net::IpAddr;

use bincode::{Decode, Encode};

use crate::{ConnectionID, Port};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NewTcpConnection {
    pub connection_id: ConnectionID,
    pub address: IpAddr,
    pub destination_port: Port,
    pub source_port: Port,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct TcpData {
    pub connection_id: ConnectionID,
    pub bytes: Vec<u8>,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct TcpClose {
    pub connection_id: ConnectionID,
}

/// Messages related to Tcp handler from client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcp {
    PortSubscribe(Port),
    ConnectionUnsubscribe(ConnectionID),
    PortUnsubscribe(Port),
}

/// Messages related to Tcp handler from server.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonTcp {
    NewConnection(NewTcpConnection),
    Data(TcpData),
    Close(TcpClose),
    /// Used to notify the subscription occured
    Subscribed,
}
