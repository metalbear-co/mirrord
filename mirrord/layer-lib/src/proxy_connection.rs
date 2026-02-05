//! Common ProxyConnection implementation shared between Unix and Windows layers.

#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::{
    collections::HashMap,
    ffi::OsString,
    fmt::Debug,
    io,
    net::{SocketAddr, TcpStream},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use mirrord_intproxy_protocol::{
    IsLayerRequest, IsLayerRequestWithResponse, LayerId, LayerToProxyMessage, LocalMessage,
    MessageId, NewSessionRequest, ProxyToLayerMessage,
    codec::{self, CodecError, SyncDecoder, SyncEncoder},
};
#[cfg(unix)]
use nix::errno::Errno;
use thiserror::Error;

use crate::error::{HookError, HookResult};

/// Global proxy connection instance, shared across the layer
pub static PROXY_CONNECTION: std::sync::OnceLock<ProxyConnection> = std::sync::OnceLock::new();

pub static INTPROXY_CONN_FD_ENV_VAR: &str = "MIRRORD_INTPROXY_CONNECTION_FD";

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
    #[error("{INTPROXY_CONN_FD_ENV_VAR} is set to an invalid value: {0:?}")]
    BadProxyConnFdEnv(OsString),
    #[cfg(unix)]
    #[error(
        "fd passed in {INTPROXY_CONN_FD_ENV_VAR} ({fd:?}) was not a valid socket. Errno: \"{errno}\" from {fn_name}"
    )]
    BadProxyConnFd {
        fd: OwnedFd,
        errno: Errno,
        fn_name: &'static str,
    },
}

impl<T> From<PoisonError<T>> for ProxyError {
    fn from(_value: PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}

pub type Result<T, E = ProxyError> = std::result::Result<T, E>;

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
        if let ProxyToLayerMessage::NewSession(layer_id) = &response {
            Ok(Self {
                sender: Mutex::new(sender),
                responses: Mutex::new(responses),
                next_message_id: AtomicU64::new(1),
                layer_id: *layer_id,
                proxy_addr,
            })
        } else {
            Err(ProxyError::UnexpectedResponse(Box::new(response)))
        }
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

    pub fn make_request_with_response<T>(&self, request: T) -> Result<T::Response>
    where
        T: IsLayerRequestWithResponse + Debug,
        T::Response: Debug,
    {
        let response_id = self.send(request.wrap())?;
        let response = self.receive(response_id)?;
        T::try_unwrap_response(response).map_err(|e| ProxyError::UnexpectedResponse(Box::new(e)))
    }

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

/// Generic helper to make proxy requests with consistent error handling
pub fn make_proxy_request_with_response<T>(request: T) -> Result<T::Response>
where
    T: IsLayerRequestWithResponse + Debug,
    T::Response: Debug,
{
    PROXY_CONNECTION
        .get()
        .ok_or_else(|| {
            ProxyError::IoFailed(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "Cannot get proxy connection",
            ))
        })?
        .make_request_with_response(request)
}

/// Generic helper function to proxy a request that might have a large response to mirrord intproxy.
/// Blocks until the request is sent.
pub fn make_proxy_request_no_response<T: IsLayerRequest + Debug>(
    request: T,
) -> HookResult<MessageId> {
    // SAFETY: mutation happens only on initialization.
    #[allow(static_mut_refs)]
    PROXY_CONNECTION
        .get()
        .ok_or_else(|| {
            HookError::ProxyError(ProxyError::IoFailed(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "Cannot get proxy connection",
            )))
        })?
        .make_request_no_response(request)
        .map_err(|e| e.into())
}
