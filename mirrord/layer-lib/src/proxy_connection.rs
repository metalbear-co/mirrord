//! Common ProxyConnection implementation shared between Unix and Windows layers.
use std::{
    collections::HashMap,
    fmt::Debug,
    io,
    net::{SocketAddr, TcpStream},
    sync::{
        OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use mirrord_intproxy_protocol::{
    IsLayerRequest, IsLayerRequestWithResponse, LayerId, LayerToProxyMessage, LocalMessage,
    MessageId, NewSessionRequest, ProxyToLayerMessage,
    codec::{self, CodecError, SyncDecoder, SyncEncoder},
};
use thiserror::Error;

use crate::{
    error::{HookError, HookResult},
    mutex::Mutex,
};

// TODO: We don't really need a lock, we just need a type that:
//  1. Can be initialized as static (with a const constructor or whatever)
//  2. Is `Sync` (because shared static vars have to be).
//  3. Can replace the held [`ProxyConnection`] with a different one (because we need to reset it on
//     `fork`).
//  We only ever set it in the ctor or in the `fork` hook (in the child process), and in both cases
//  there are no other threads yet in that process, so we don't need write synchronization.
//  Assuming it's safe to call `send` simultaneously from two threads, on two references to the
//  same `Sender` (is it), we also don't need read synchronization.
/// Global connection to the internal proxy.
/// Should not be used directly. Use [`make_proxy_request_with_response`] or
/// [`make_proxy_request_no_response`] functions instead.
pub static mut PROXY_CONNECTION: OnceLock<ProxyConnection> = OnceLock::new();

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("{0}")]
    CodecError(#[from] CodecError),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("unexpected response: {0:?}")]
    UnexpectedResponse(
        /// Boxed due to large size difference.
        Box<ProxyToLayerMessage>,
    ),
    #[error("critical error: {0}")]
    ProxyFailure(String),
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
            return Err(ProxyError::UnexpectedResponse(Box::new(response)));
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

    pub fn receive(&self, response_id: u64) -> Result<ProxyToLayerMessage> {
        let response = self.responses.lock()?.receive(response_id)?;
        match response {
            ProxyToLayerMessage::ProxyFailed(error_msg) => Err(ProxyError::ProxyFailure(error_msg)),
            _ => Ok(response),
        }
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_with_response<T>(&self, request: T) -> Result<T::Response>
    where
        T: IsLayerRequestWithResponse + Debug,
        T::Response: Debug,
    {
        let response_id = self.send(request.wrap())?;
        let response = self.receive(response_id)?;
        T::try_unwrap_response(response)
            .map_err(Box::new)
            .map_err(ProxyError::UnexpectedResponse)
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    pub fn make_request_no_response<T: IsLayerRequest + Debug>(
        &self,
        request: T,
    ) -> Result<MessageId> {
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

            self.outstanding_responses
                .insert(response.message_id, response.inner);
        }
    }
}

/// Makes a request to the internal proxy using global [`PROXY_CONNECTION`].
/// Blocks until the proxy responds.
pub fn make_proxy_request_with_response<T>(request: T) -> HookResult<T::Response>
where
    T: IsLayerRequestWithResponse + Debug,
    T::Response: Debug,
{
    // SAFETY: mutation happens only on initialization.
    #[allow(static_mut_refs)]
    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or(HookError::CannotGetProxyConnection)?
            .make_request_with_response(request)
            .map_err(Into::into)
    }
}

/// Makes a request to the internal proxy using global [`PROXY_CONNECTION`].
/// Blocks until the request is sent.
pub fn make_proxy_request_no_response<T: IsLayerRequest + Debug>(
    request: T,
) -> HookResult<MessageId> {
    // SAFETY: mutation happens only on initialization.
    #[allow(static_mut_refs)]
    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or(HookError::CannotGetProxyConnection)?
            .make_request_no_response(request)
            .map_err(Into::into)
    }
}
