#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NewTCPConnection {
    pub connection_id: ConnectionID,
    pub address: IpAddr,
    pub destination_port: Port,
    pub source_port: Port,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct TCPData {
    pub connection_id: ConnectionID,
    pub data: Vec<u8>,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct TCPClose {
    pub connection_id: ConnectionID,
}