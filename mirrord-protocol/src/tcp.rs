use std::net::{IpAddr, SocketAddr};

use bincode::{Decode, Encode};

use crate::{ConnectionID, Port, RemoteResult};

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

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ConnectRequest {
    pub remote_address: SocketAddr,
}

/// Messages related to Tcp handler from client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcp {
    PortSubscribe(Port),
    ConnectionUnsubscribe(ConnectionID),
    PortUnsubscribe(Port),
    ConnectRequest(ConnectRequest),
}

/// Messages related to Tcp handler from server.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonTcp {
    NewConnection(NewTcpConnection),
    Data(TcpData),
    Close(TcpClose),
}
