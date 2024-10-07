use std::{
    collections::HashMap,
    fmt, io,
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, PoisonError,
    },
    time::Duration,
};

use mirrord_intproxy_protocol::{
    codec::{self, CodecError, SyncDecoder, SyncEncoder},
    IsLayerRequest, IsLayerRequestWithResponse, LayerId, LayerToProxyMessage, LocalMessage,
    MessageId, NewSessionRequest, ProxyToLayerMessage,
};
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
    layer_id: LayerId,
    proxy_addr: SocketAddr,
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
        let ProxyToLayerMessage::NewSession(layer_id) = &response else {
            return Err(ProxyError::UnexpectedResponse(response));
        };

        Ok(Self {
            sender: Mutex::new(sender),
            responses: Mutex::new(responses),
            next_message_id: AtomicU64::new(1),
            layer_id: *layer_id,
            proxy_addr,
        })
    }

    fn next_message_id(&self) -> MessageId {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn send(&self, message: LayerToProxyMessage) -> Result<MessageId> {
        let message_id = self.next_message_id();
        let message = LocalMessage {
            message_id,
            inner: message,
        };

        let mut guard = self.sender.lock()?;
        guard.send(&message)?;
        guard.flush()?;

        Ok(message_id)
    }

    pub fn receive(&self, response_id: MessageId) -> Result<ProxyToLayerMessage> {
        self.responses.lock()?.receive(response_id)
    }

    pub fn receive_into_response<T>(&self, response_id: MessageId) -> Result<T::Response>
    where
        T: IsLayerRequestWithResponse,
    {
        let response = self.receive(response_id)?;
        T::try_unwrap_response(response).map_err(ProxyError::UnexpectedResponse)
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_with_response<T>(&self, request: T) -> Result<T::Response>
    where
        T: IsLayerRequestWithResponse + fmt::Debug,
        T::Response: fmt::Debug,
    {
        let response_id = self.send(request.wrap())?;
        self.receive_into_response::<T>(response_id)
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_with_response_and_id<T>(
        &self,
        request: T,
    ) -> Result<(MessageId, T::Response)>
    where
        T: IsLayerRequestWithResponse + fmt::Debug,
        T::Response: fmt::Debug,
    {
        let response_id = self.send(request.wrap())?;
        self.receive_into_response::<T>(response_id)
            .map(|response| (response_id, response))
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_no_response<T: IsLayerRequest + fmt::Debug>(
        &self,
        request: T,
    ) -> Result<MessageId> {
        self.send(request.wrap())
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self, handler), ret)]
    pub fn queue_handler(
        &self,
        response_id: MessageId,
        handler: impl FnOnce(ProxyToLayerMessage) + 'static,
    ) -> Result<()> {
        let mut guard = self.responses.lock()?;

        if let Some(response) = guard.outstanding_responses.remove(&response_id) {
            handler(response);
        } else {
            guard
                .outstanding_handlers
                .insert(response_id, Box::new(handler));
        }

        Ok(())
    }

    pub fn layer_id(&self) -> LayerId {
        self.layer_id
    }

    pub fn proxy_addr(&self) -> SocketAddr {
        self.proxy_addr
    }
}

struct ResponseManager {
    receiver: SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>,
    outstanding_responses: HashMap<u64, ProxyToLayerMessage>,
    outstanding_handlers: HashMap<u64, Box<dyn FnOnce(ProxyToLayerMessage)>>,
}

impl ResponseManager {
    fn new(receiver: SyncDecoder<LocalMessage<ProxyToLayerMessage>, TcpStream>) -> Self {
        Self {
            receiver,
            outstanding_responses: Default::default(),
            outstanding_handlers: Default::default(),
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

            if let Some(handler) = self.outstanding_handlers.remove(&response.message_id) {
                handler(response.inner);
            } else {
                self.outstanding_responses
                    .insert(response.message_id, response.inner);
            }
        }
    }
}

impl fmt::Debug for ResponseManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseManager")
            .field("receiver", &self.receiver)
            .field("outstanding_responses", &self.outstanding_responses)
            .field(
                "outstanding_handlers",
                &format!("HashMap({})", self.outstanding_responses.len()),
            )
            .finish()
    }
}
