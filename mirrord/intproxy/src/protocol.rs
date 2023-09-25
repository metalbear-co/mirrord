use bincode::{Decode, Encode};

#[derive(Encode, Decode, Debug)]
pub struct LocalMessage<T> {
    /// [`None`] for proxy -> layer messages that are not responses.
    pub message_id: Option<u64>,
    pub inner: T,
}

pub type SessionId = u64;

#[derive(Encode, Decode, Debug)]
pub enum InitSession {
    New,
    ForkedFrom(SessionId),
    ExecFrom(SessionId),
}

#[derive(Encode, Decode, Debug)]
pub enum LayerToProxyMessage {
    InitSession(InitSession),
}

#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    SessionInfo(SessionId),
}
