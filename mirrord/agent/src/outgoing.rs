use std::{collections::VecDeque, io, sync::Arc};

use bytes::Bytes;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, outgoing::SocketAddress, uid::Uid,
};
use thiserror::Error;
use tokio::{sync::OwnedSemaphorePermit, task::JoinError};

use crate::{
    error::AgentError,
    outgoing::router::{ConnEvent, ConnectionKind, OutgoingRouter, RouterUpdate},
    task::BgTaskRuntime,
    util::io::throttle::Throttle,
};

pub mod router;
pub mod seqpacket;
pub mod tcp_unix;
pub mod udp;

/// API for handling outgoing traffic logic for one specific [`OutgoingKind`].
///
/// This struct handles all of the logic, you only need to feed it with client messages
/// and read daemon messages from it.
pub struct OutgoingApi<O: OutgoingKind> {
    router: OutgoingRouter<O>,
    /// Queued legacy [`LayerConnect`](mirrord_protocol::outgoing::LayerConnect) requests.
    ///
    /// The client expects responses in the same order.
    /// Since this is a backwards compatibility thing, we don't put much effort into it.
    /// [`OutgoingRouter`] operates only on connect requests with [`Uid`]s,
    /// while we end the legacy logic here, by adding mock [`Uid`]s to the requests.
    /// We pass the requests to the router one by one.
    ///
    /// **Important**: the first request is already in the router.
    queued_connects: VecDeque<(Uid, SocketAddress)>,
}

impl<O: OutgoingKind> OutgoingApi<O> {
    /// Creates a new API instance.
    ///
    /// # Params
    ///
    /// * `network_runtime` - [`tokio`] runtime where connections will be made. If this agent has a
    ///   target, this runtime has to run in its network namespace.
    /// * `inbound_throttle` - will be used to throttle traffic *from* the peers.
    /// * `outbound_throttle` - will be used to throttle traffic *to* the peers.
    pub fn new(
        network_runtime: Arc<BgTaskRuntime>,
        inbound_throttle: Throttle,
        outbound_throttle: Throttle,
    ) -> Self {
        Self {
            router: OutgoingRouter::new(network_runtime, inbound_throttle, outbound_throttle),
            queued_connects: Default::default(),
        }
    }

    /// Handles the given client message.
    ///
    /// This method is async, and **may block** on IO for **finite** amount of time.
    pub async fn handle_message(&mut self, message: O::InMessage) {
        match O::transform_in(message) {
            GenericInMessage::ConnectLegacy(socket_address) => {
                let uid = Uid::new_v4();
                let is_first = self.queued_connects.is_empty();
                let queued = self.queued_connects.push_back_mut((uid, socket_address));
                if is_first {
                    self.router.start_connect(uid, queued.1.clone());
                }
            }
            GenericInMessage::Connect(uid, socket_address) => {
                self.router.start_connect(uid, socket_address);
            }
            GenericInMessage::Write(id, bytes) if bytes.is_empty() => {
                self.router.close_writing(id).await;
            }
            GenericInMessage::Write(id, bytes) => {
                self.router.write(id, bytes).await;
            }
            GenericInMessage::Close(id) => {
                self.router.close(id).await;
            }
        }
    }

    /// Receives the next [`DaemonMessage`] produced by this API.
    ///
    /// The [`OwnedSemaphorePermit`] comes from one of the inbound [`Throttle`] instances,
    /// and should be dropped **only** after the message is removed from the memory.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel safe, and can be used in a [`tokio::select`] branch.
    pub async fn recv(
        &mut self,
    ) -> Option<Result<(DaemonMessage, Option<OwnedSemaphorePermit>), OutgoingError>> {
        let update = match self.router.recv().await? {
            Ok(update) => update,
            Err(error) => return Some(Err(error)),
        };

        // Important - no `.await` after this point.
        // We promised cancellation safety.

        match update {
            RouterUpdate::ConnectOk {
                uid,
                id,
                local_addr,
                peer_addr,
            } => {
                let was_ordered = self
                    .queued_connects
                    .pop_front_if(|queued| queued.0 == uid)
                    .is_some();
                let uid = if was_ordered {
                    if let Some(next) = self.queued_connects.front() {
                        self.router.start_connect(next.0, next.1.clone());
                    }
                    None
                } else {
                    Some(uid)
                };
                let message = GenericOutMessage::ConnectOk {
                    uid,
                    id,
                    local_addr,
                    peer_addr,
                };
                Some(Ok((O::transform_out(message), None)))
            }
            RouterUpdate::ConnectErr { uid, error } => {
                let was_ordered = self
                    .queued_connects
                    .pop_front_if(|queued| queued.0 == uid)
                    .is_some();
                let uid = if was_ordered {
                    if let Some(next) = self.queued_connects.front() {
                        self.router.start_connect(next.0, next.1.clone());
                    }
                    None
                } else {
                    Some(uid)
                };
                let message = GenericOutMessage::ConnectErr { uid, error };
                Some(Ok((O::transform_out(message), None)))
            }
            RouterUpdate::ConnEvent {
                id,
                event: ConnEvent::ReadData(data),
            } => {
                let (data, permit) = data.unpack();
                let message = O::transform_out(GenericOutMessage::Read(id, data));
                Some(Ok((message, Some(permit))))
            }
            RouterUpdate::ConnEvent {
                id,
                event: ConnEvent::ReadClosed,
            } => {
                let message = O::transform_out(GenericOutMessage::Read(id, Bytes::new()));
                Some(Ok((message, None)))
            }
            RouterUpdate::ConnEvent {
                id,
                event: ConnEvent::Failed(error),
            } => Some(Ok((
                DaemonMessage::LogMessage(LogMessage::warn(format!(
                    "outgoing connection {id} failed: {error} ({})",
                    std::any::type_name::<O>(),
                ))),
                None,
            ))),
            RouterUpdate::ConnEvent {
                id,
                event: ConnEvent::FullyClosed,
            } => {
                let message = O::transform_out(GenericOutMessage::Close(id));
                Some(Ok((message, None)))
            }
        }
    }
}

/// Errors from [`OutgoingApi`].
///
/// All are fatal.
#[derive(Error, Debug)]
pub enum OutgoingError {
    #[error("exhausted u64 connection IDs")]
    ExhaustedConnIds,
    #[error("connect task panicked: {0}")]
    ConnectPanic(#[from] JoinError),
}

impl From<OutgoingError> for AgentError {
    fn from(value: OutgoingError) -> Self {
        match value {
            OutgoingError::ExhaustedConnIds => AgentError::ExhaustedConnectionId,
            OutgoingError::ConnectPanic(error) => AgentError::BackgroundTaskFailed {
                task: "outgoing_connect",
                error: Arc::new(error),
            },
        }
    }
}

/// Kind of outgoing traffic that the agent can serve.
pub trait OutgoingKind: ConnectionKind {
    /// Type of mirrord-protocol client message for this outgoing kind.
    type InMessage;

    fn transform_in(message: Self::InMessage) -> GenericInMessage;

    fn transform_out(message: GenericOutMessage) -> DaemonMessage;
}

/// Generic client message consumed by the outgoing logic.
///
/// All client messages for all [`OutgoingKind`]s can be transformed into this enum.
pub enum GenericInMessage {
    ConnectLegacy(SocketAddress),
    Connect(Uid, SocketAddress),
    Write(
        ConnectionId,
        /// Empty means shutdown from the client side.
        Bytes,
    ),
    Close(ConnectionId),
}

/// Generic message produced by the outgoing logic.
///
/// Can be transformed into a [`DaemonMessage`] specific to some [`OutgoingKind`].
pub enum GenericOutMessage {
    ConnectOk {
        uid: Option<Uid>,
        id: ConnectionId,
        local_addr: SocketAddress,
        peer_addr: SocketAddress,
    },
    ConnectErr {
        uid: Option<Uid>,
        error: io::Error,
    },
    Read(
        ConnectionId,
        /// Empty means shutdown from the peer side.
        Bytes,
    ),
    Close(ConnectionId),
}
