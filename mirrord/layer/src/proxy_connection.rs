use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, PoisonError,
    },
    time::Duration,
};

use mirrord_intproxy::{
    codec::{self, CodecError, SyncDecoder, SyncEncoder},
    protocol::{
        IsLayerRequest, IsLayerRequestWithResponse, LayerToProxyMessage, LocalMessage, MessageId,
        NewSessionRequest, ProxyToLayerMessage, SessionId, NOT_A_RESPONSE,
    },
};
use mirrord_protocol::LogLevel;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("{0}")]
    CodecError(#[from] CodecError),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("unexpected response: {0:?}")]
    UnexpectedResponse(ProxyToLayerMessage),
    #[error("connection lock poisoned")]
    LockPoisoned,
    #[error("{0}")]
    IoFailed(#[from] io::Error),
    #[error("no remote address sent for new incoming connection")]
    NoRemoteAddress,
}

impl<T> From<PoisonError<T>> for ProxyError {
    fn from(_value: PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}

pub type Result<T> = core::result::Result<T, ProxyError>;

#[derive(Debug)]
pub struct ProxyConnection {
    sender: Mutex<SyncEncoder<LocalMessage<LayerToProxyMessage>, TcpStream>>,
    responses: Mutex<ResponseManager>,
    next_message_id: AtomicU64,
    session_id: SessionId,
}

impl ProxyConnection {
    pub fn new(
        proxy_addr: SocketAddr,
        session: NewSessionRequest,
        timeout: Duration,
    ) -> Result<Self> {
        let connection = TcpStream::connect(proxy_addr)?;
        connection.set_read_timeout(Some(timeout))?;
        connection.set_write_timeout(Some(timeout))?;

        let (mut sender, receiver) = codec::make_sync_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(connection)?;

        sender.send(&LocalMessage {
            message_id: 0,
            inner: LayerToProxyMessage::NewSession(session),
        })?;

        let mut responses = ResponseManager::new(receiver);
        let response = responses.receive(0)?;
        let ProxyToLayerMessage::NewSession(session_id) = &response else {
            return Err(ProxyError::UnexpectedResponse(response));
        };

        Ok(Self {
            sender: Mutex::new(sender),
            responses: Mutex::new(responses),
            next_message_id: AtomicU64::new(1),
            session_id: *session_id,
        })
    }

    fn next_message_id(&self) -> MessageId {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn send(&self, message: LayerToProxyMessage) -> Result<MessageId> {
        let message_id = loop {
            match self.next_message_id() {
                NOT_A_RESPONSE => continue,
                other => break other,
            }
        };

        let message = LocalMessage {
            message_id,
            inner: message,
        };

        let mut guard = self.sender.lock()?;
        guard.send(&message)?;
        guard.flush()?;

        Ok(message_id)
    }

    pub fn receive(&self, response_id: u64) -> Result<ProxyToLayerMessage> {
        self.responses.lock()?.receive(response_id)
    }

    pub fn make_request_with_response<T: IsLayerRequestWithResponse>(
        &self,
        request: T,
    ) -> Result<T::Response> {
        let response_id = self.send(request.wrap())?;
        let response = self.receive(response_id)?;
        T::try_unwrap_response(response).map_err(ProxyError::UnexpectedResponse)
    }

    pub fn make_request_no_response<T: IsLayerRequest>(&self, request: T) -> Result<MessageId> {
        self.send(request.wrap())
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

#[derive(Debug)]
struct ResponseManager {
    receiver: SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>,
    outstanding_responses: HashMap<u64, ProxyToLayerMessage>,
}

impl ResponseManager {
    fn new(receiver: SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>) -> Self {
        Self {
            receiver,
            outstanding_responses: Default::default(),
        }
    }

    fn receive(&mut self, response_id: u64) -> Result<ProxyToLayerMessage> {
        if let Some(response) = self.outstanding_responses.remove(&response_id) {
            return Ok(response);
        }

        loop {
            let response = self
                .receiver
                .receive()?
                .ok_or(ProxyError::ConnectionClosed)?;

            if response.message_id == response_id {
                break Ok(response.inner);
            }

            if response.message_id == NOT_A_RESPONSE {
                Self::handle_custom_message(response.inner);
            } else {
                self.outstanding_responses
                    .insert(response.message_id, response.inner);
            }
        }
    }

    fn handle_custom_message(message: ProxyToLayerMessage) {
        if let ProxyToLayerMessage::AgentLog(log) = message {
            match log.level {
                LogLevel::Warn => tracing::warn!("agent warning: {}", log.message),
                LogLevel::Error => tracing::error!("agent error: {}", log.message),
            }
        }
    }
}
