use std::{
    collections::HashMap,
    env::VarError,
    ffi::OsString,
    fmt::Debug,
    io,
    net::{SocketAddr, TcpStream},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::prelude::RawFd,
    },
    sync::{
        PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use libc::{F_GETFD, F_SETFD, FD_CLOEXEC, fcntl};
use mirrord_intproxy_protocol::{
    IsLayerRequest, IsLayerRequestWithResponse, LayerId, LayerToProxyMessage, LocalMessage,
    MessageId, NewSessionRequest, ProxyToLayerMessage,
    codec::{self, CodecError, SyncDecoder, SyncEncoder},
};
use nix::{
    errno::Errno,
    sys::socket::{SockaddrStorage, getpeername, getsockopt},
};
use thiserror::Error;

use crate::{detour::DetourGuard, mutex::Mutex};

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
        let guard = DetourGuard::new();
        let connection = Self::acquire_connection(session.parent_layer.is_some(), proxy_addr)?;
        drop(guard);

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

    fn make_new_connection(proxy_addr: SocketAddr) -> Result<TcpStream> {
        let conn = TcpStream::connect(proxy_addr)?;

        let fd = conn.as_raw_fd();
        unsafe {
            let flags = fcntl(fd, F_GETFD);
            fcntl(fd, F_SETFD, flags & (!FD_CLOEXEC));
        }
        Ok(conn)
    }

    fn acquire_connection(have_parent_layer: bool, proxy_addr: SocketAddr) -> Result<TcpStream> {
        // Parent layer is Some OR we don't have the env var set =>
        // We're calling this from a fork(), i.e. this is a a brand
        // new process and we have to establish a new connection to
        // the intproxy.
        if have_parent_layer {
            return Self::make_new_connection(proxy_addr);
        }

        let fd: i32 = match std::env::var(INTPROXY_CONN_FD_ENV_VAR) {
            Ok(var) => var
                .parse()
                .map_err(|_| ProxyError::BadProxyConnFdEnv(var.into()))?,
            Err(VarError::NotPresent) => return Self::make_new_connection(proxy_addr),
            Err(VarError::NotUnicode(os)) => return Err(ProxyError::BadProxyConnFdEnv(os)),
        };

        let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };

        // Ensure the fd is a valid socket.
        if let Err(errno) = getsockopt(&owned_fd, nix::sys::socket::sockopt::SockType) {
            return Err(ProxyError::BadProxyConnFd {
                fd: owned_fd,
                errno,
                fn_name: "getsockopt",
            });
        }

        if let Err(errno) = getpeername::<SockaddrStorage>(fd) {
            return Err(ProxyError::BadProxyConnFd {
                fd: owned_fd,
                errno,
                fn_name: "getpeername",
            });
        }

        // Recover the connection
        Ok(TcpStream::from(owned_fd))
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

impl AsRawFd for ProxyConnection {
    fn as_raw_fd(&self) -> RawFd {
        self.sender.lock().unwrap().as_ref().as_raw_fd()
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
