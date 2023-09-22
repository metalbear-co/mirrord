use bincode::{Decode, Encode};
use mirrord_protocol::{ClientMessage, DaemonMessage};

#[derive(Encode, Decode, Debug)]
pub enum LayerToProxyMessage {
    ClientMessage(ClientMessage),
}

#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    DaemonMessage(DaemonMessage),
}
