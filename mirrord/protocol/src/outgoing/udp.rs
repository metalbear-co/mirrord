use super::*;
use crate::{Port, RemoteResult};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerUdpOutgoing {
    Connect(LayerConnect),
    Write(LayerWrite),
    Close(LayerClose),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonUdpOutgoing {
    Connect(RemoteResult<DaemonConnect>),
    Read(RemoteResult<DaemonRead>),
    Close(ConnectionId),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct SendMsgResponse {
    pub sent_amount: usize,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct SendMsgRequest {
    pub message: String,
    pub addr: Option<String>,
    pub bound: Option<BoundAddress>,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct BoundAddress {
    pub requested_port: Port,
    pub address: SocketAddr,
}
