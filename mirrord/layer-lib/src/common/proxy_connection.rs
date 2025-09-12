//! Common ProxyConnection implementation shared between Unix and Windows layers.

use std::{
    collections::HashMap,
    fmt::Debug,
    net::{SocketAddr, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use mirrord_intproxy_protocol::{
    IsLayerRequest, IsLayerRequestWithResponse, LayerId, LayerToProxyMessage, LocalMessage,
    MessageId, NewSessionRequest, ProxyToLayerMessage,
    codec::{self, SyncDecoder, SyncEncoder},
};

use crate::error::{HookError, HookResult, ProxyError, ProxyResult};

/// Efficient result type for internal proxy operations to reduce memory usage
/// Now that HookResult uses Box<HookError> by default, we can use it directly
type ProxyConnResult<T> = HookResult<T>;

/// Global proxy connection instance, shared across the layer
pub static PROXY_CONNECTION: std::sync::OnceLock<ProxyConnection> = std::sync::OnceLock::new();

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
    ) -> ProxyResult<Self> {
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
        if let ProxyToLayerMessage::NewSession(layer_id) = &response {
            Ok(Self {
                sender: Mutex::new(sender),
                responses: Mutex::new(responses),
                next_message_id: AtomicU64::new(1),
                layer_id: *layer_id,
                proxy_addr,
            })
        } else {
            Err(Box::new(ProxyError::UnexpectedResponse(response)))
        }
    }

    fn next_message_id(&self) -> MessageId {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn send(&self, message: LayerToProxyMessage) -> ProxyResult<MessageId> {
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

    pub fn receive(&self, response_id: u64) -> ProxyResult<ProxyToLayerMessage> {
        let response = self.responses.lock()?.receive(response_id)?;
        match response {
            ProxyToLayerMessage::ProxyFailed(error_msg) => {
                Err(Box::new(ProxyError::ProxyFailure(error_msg)))
            }
            _ => Ok(response),
        }
    }

    pub fn make_request_with_response<T>(&self, request: T) -> ProxyResult<T::Response>
    where
        T: IsLayerRequestWithResponse + Debug,
        T::Response: Debug,
    {
        let response_id = self.send(request.wrap())?;
        let response = self.receive(response_id)?;
        T::try_unwrap_response(response).map_err(|e| Box::new(ProxyError::UnexpectedResponse(e)))
    }

    pub fn make_request_no_response<T: IsLayerRequest + Debug>(
        &self,
        request: T,
    ) -> ProxyResult<MessageId> {
        self.send(request.wrap())
    }

    pub fn layer_id(&self) -> LayerId {
        self.layer_id
    }

    pub fn proxy_addr(&self) -> SocketAddr {
        self.proxy_addr
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

    fn receive(&mut self, response_id: u64) -> ProxyResult<ProxyToLayerMessage> {
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

            self.outstanding_responses
                .insert(response.message_id, response.inner);
        }
    }
}

/// Generic helper to make proxy requests with consistent error handling
pub fn make_proxy_request_with_response<T>(request: T) -> HookResult<T::Response>
where
    T: IsLayerRequestWithResponse + Debug,
    T::Response: Debug,
{
    make_proxy_request_with_response_impl(request)
}

/// Internal implementation using ProxyConnResult for memory efficiency
fn make_proxy_request_with_response_impl<T>(request: T) -> ProxyConnResult<T::Response>
where
    T: IsLayerRequestWithResponse + Debug,
    T::Response: Debug,
{
    PROXY_CONNECTION
        .get()
        .ok_or_else(|| {
            Box::new(HookError::ProxyError(ProxyError::IoFailed(
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "Cannot get proxy connection",
                ),
            )))
        })?
        .make_request_with_response(request)
        .map_err(|e| e.into())
}

/// Generic helper function to proxy a request that might have a large response to mirrord intproxy.
/// Blocks until the request is sent.
pub fn make_proxy_request_no_response<T: IsLayerRequest + Debug>(
    request: T,
) -> HookResult<MessageId> {
    make_proxy_request_no_response_impl(request)
}

/// Internal implementation using ProxyConnResult for memory efficiency
fn make_proxy_request_no_response_impl<T: IsLayerRequest + Debug>(
    request: T,
) -> ProxyConnResult<MessageId> {
    // SAFETY: mutation happens only on initialization.
    #[allow(static_mut_refs)]
    PROXY_CONNECTION
        .get()
        .ok_or_else(|| {
            Box::new(HookError::ProxyError(ProxyError::IoFailed(
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "Cannot get proxy connection",
                ),
            )))
        })?
        .make_request_no_response(request)
        .map_err(|e| e.into())
}
