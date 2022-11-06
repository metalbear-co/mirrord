use super::*;
use crate::RemoteResult;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcpOutgoing {
    Connect(LayerConnect),
    Write(LayerWrite),
    Close(LayerClose),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonTcpOutgoing {
    Connect(RemoteResult<DaemonConnect>),
    Read(RemoteResult<DaemonRead>),
    Close(ConnectionId),
}
