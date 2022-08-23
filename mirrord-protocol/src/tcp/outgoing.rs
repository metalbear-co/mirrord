use core::fmt;
use std::net::SocketAddr;

use bincode::{Decode, Encode};

use crate::{ConnectionId, RemoteResult};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ConnectRequest {
    pub remote_address: SocketAddr,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ReadRequest {
    pub connection_id: ConnectionId,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct WriteRequest {
    pub connection_id: ConnectionId,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for WriteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteRequest")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum TcpOutgoingRequest {
    Connect(ConnectRequest),
    Write(WriteRequest),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ConnectResponse {
    pub connection_id: ConnectionId,
    pub remote_address: SocketAddr,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct ReadResponse {
    pub connection_id: ConnectionId,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for ReadResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadResponse")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct WriteResponse {
    pub connection_id: ConnectionId,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum TcpOutgoingResponse {
    Connect(RemoteResult<ConnectResponse>),
    Read(RemoteResult<ReadResponse>),
    Write(RemoteResult<WriteResponse>),
}
