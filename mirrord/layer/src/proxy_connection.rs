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
    codec::{self, CodecError, SyncReceiver, SyncSender},
    protocol::{
        hook::{
            Access, FdOpenDir, FileOperation, GetDEnts64, HookMessage, Open, OpenRelative, Read,
            ReadDir, ReadLimited, Seek, Write, WriteLimited, Xstat, XstatFs,
        },
        HasResponse, InitSession, LayerToProxyMessage, LocalMessage, ProxyToLayerMessage,
    },
};
use mirrord_protocol::{
    file::{
        AccessFileResponse, GetDEnts64Response, OpenDirResponse, OpenFileResponse, ReadDirResponse,
        ReadFileResponse, SeekFileResponse, WriteFileResponse, XstatFsResponse, XstatResponse,
    },
    FileResponse, RemoteResult,
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
    sender: Mutex<SyncSender<LocalMessage<LayerToProxyMessage>, TcpStream>>,
    responses: Mutex<ResponseManager>,
    next_message_id: AtomicU64,
    session_id: u64,
}

impl ProxyConnection {
    pub fn new(proxy_addr: SocketAddr, session: InitSession, timeout: Duration) -> Result<Self> {
        let connection = TcpStream::connect(proxy_addr)?;
        connection.set_read_timeout(Some(timeout))?;
        connection.set_write_timeout(Some(timeout))?;

        let (mut sender, receiver) = codec::make_sync_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(connection)?;

        sender.send(&LocalMessage {
            message_id: 0,
            inner: LayerToProxyMessage::InitSession(session),
        })?;

        let mut responses = ResponseManager::new(receiver);
        let response = responses.receive(0)?;
        let ProxyToLayerMessage::SessionInfo(session_id) = &response else {
            return Err(ProxyError::UnexpectedResponse(response));
        };

        Ok(Self {
            sender: Mutex::new(sender),
            responses: Mutex::new(responses),
            next_message_id: AtomicU64::new(1),
            session_id: *session_id,
        })
    }

    fn next_message_id(&self) -> u64 {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn send(&self, message: LayerToProxyMessage) -> Result<u64> {
        let message_id = self.next_message_id();
        let message = LocalMessage {
            message_id,
            inner: message,
        };

        self.sender.lock()?.send(&message)?;

        Ok(message_id)
    }

    pub fn receive(&self, response_id: u64) -> Result<ProxyToLayerMessage> {
        self.responses.lock()?.receive(response_id)
    }

    pub fn make_request<T: HasResponse>(&self, request: T) -> Result<T::Response> {
        let response_id = self.send(request.into_message())?;
        let response = self.receive(response_id)?;
        T::extract_response(response).map_err(ProxyError::UnexpectedResponse)
    }
}

#[derive(Debug)]
struct ResponseManager {
    receiver: SyncReceiver<LocalMessage<ProxyToLayerMessage>, TcpStream>,
    outstanding_responses: HashMap<u64, ProxyToLayerMessage>,
}

impl ResponseManager {
    fn new(receiver: SyncReceiver<LocalMessage<ProxyToLayerMessage>, TcpStream>) -> Self {
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
