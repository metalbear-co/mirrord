use core::fmt;
use std::net::SocketAddr;

use bincode::{Decode, Encode};

use crate::RemoteResult;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ConnectRequest {
    pub user_fd: i32,
    pub remote_address: SocketAddr,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ReadRequest {
    pub id: i32,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct WriteRequest {
    pub id: i32,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for WriteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteRequest")
            .field("id", &self.id)
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
    pub user_fd: i32,
    pub remote_address: SocketAddr,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct ReadResponse {
    pub id: i32,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for ReadResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadResponse")
            .field("id", &self.id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct WriteResponse {
    pub id: i32,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum TcpOutgoingResponse {
    Connect(RemoteResult<ConnectResponse>),
    Read(RemoteResult<ReadResponse>),
    Write(RemoteResult<WriteResponse>),
}
