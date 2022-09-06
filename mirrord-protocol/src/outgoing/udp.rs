use super::*;
use crate::RemoteResult;

/// `user` wants to connect to `remote_address`.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LayerUdpConnect {
    pub domain: i32,
    pub type_: i32,
    pub protocol: i32,
    pub remote_address: SocketAddr,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerUdpOutgoing {
    Connect(LayerUdpConnect),
    Write(LayerWrite),
    Close(LayerClose),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonUdpOutgoing {
    Connect(RemoteResult<DaemonConnect>),
    Read(RemoteResult<DaemonRead>),
    Close(ConnectionId),
}
