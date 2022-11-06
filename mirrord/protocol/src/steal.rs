use crate::types::*;


#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum StealClientMessage {
    PortSteal(Port),
    TCPData(TCPData),
    CloseConnection(TCPClose),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum StealDaemonMessage {
    NewConnection(NewTcpConnection),
    TCPData(TCPData),
    TCPClose(TCPClose),
}