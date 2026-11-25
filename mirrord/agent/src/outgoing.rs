use std::{collections::VecDeque, io, sync::Arc};

use bytes::Bytes;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, outgoing::SocketAddress, uid::Uid,
};
use thiserror::Error;
use tokio::task::JoinError;

use crate::{
    error::AgentError,
    outgoing::router::{ConnEvent, ConnectionKind, OutgoingRouter, RouterUpdate},
    task::BgTaskRuntime,
    util::{
        io::throttle::{Throttle, ThrottlePermit},
        target_root::TargetRoot,
    },
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
    /// * `target_root` - where the paths of Unix socket addresses are.
    /// * `inbound_throttle` - will be used to throttle traffic *from* the peers.
    /// * `outbound_throttle` - will be used to throttle traffic *to* the peers.
    pub fn new(
        network_runtime: Arc<BgTaskRuntime>,
        target_root: TargetRoot,
        inbound_throttle: Throttle,
        outbound_throttle: Throttle,
    ) -> Self {
        Self {
            router: OutgoingRouter::new(
                network_runtime,
                target_root,
                inbound_throttle,
                outbound_throttle,
            ),
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
    /// The [`ThrottlePermit`] comes from one of the inbound [`Throttle`] instances,
    /// and should be dropped **only** after the message is removed from the memory.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel safe, and can be used in a [`tokio::select`] branch.
    pub async fn recv(
        &mut self,
    ) -> Option<Result<(DaemonMessage, Option<ThrottlePermit>), OutgoingError>> {
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

#[cfg(test)]
mod test {
    use std::{
        fs,
        os::unix::{fs::symlink, net::UnixListener},
        path::{Path, PathBuf},
    };

    use mirrord_protocol::outgoing::{SocketAddress, UnixAddr};
    use rstest::rstest;
    use tokio_seqpacket::UnixSeqpacketListener;

    use crate::{
        outgoing::{
            router::ConnectionKind, seqpacket::SeqpacketConnection, tcp_unix::TcpOrUnixConnection,
        },
        util::target_root::{Resolver, TargetRoot, proc_fd_path},
    };

    #[derive(Debug, Clone, Copy)]
    enum Kind {
        Stream,
        Seqpacket,
    }

    /// Binds a listening socket at `name` in `dir`.
    ///
    /// Goes through the directory's [`proc_fd_path`], since the full path may not fit in a Unix
    /// socket address.
    fn listen(kind: Kind, dir: &Path, name: &str) -> Box<dyn std::any::Any> {
        let dir = fs::File::open(dir).unwrap();
        let path = proc_fd_path(&dir).join(name);

        match kind {
            Kind::Stream => Box::new(UnixListener::bind(path).unwrap()),
            Kind::Seqpacket => Box::new(UnixSeqpacketListener::bind(path).unwrap()),
        }
    }

    async fn connect(kind: Kind, root: &Path, resolver: Resolver, path: PathBuf) -> bool {
        let address = SocketAddress::Unix(UnixAddr::Pathname(path));
        let root = TargetRoot::with_resolver(root.to_path_buf(), resolver);

        match kind {
            Kind::Stream => TcpOrUnixConnection::connect(&address, &root).await.is_ok(),
            Kind::Seqpacket => SeqpacketConnection::connect(&address, &root).await.is_ok(),
        }
    }

    /// A Unix socket address is a path in the target root, and reaches no socket outside it:
    ///
    /// ```text
    /// outside/s.sock                  a socket outside the target root
    /// root/run/s.sock                 a socket
    /// root/<long>/s.sock              a socket whose path doesn't fit in a socket address
    /// root/linked                  -> /run/s.sock
    /// root/escaped                 -> <outside>/s.sock
    /// root/rel_escaped             -> ../outside/s.sock
    /// ```
    #[rstest]
    #[tokio::test]
    async fn unix_socket_paths_stay_in_target_root(
        #[values(Kind::Stream, Kind::Seqpacket)] kind: Kind,
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        let outside = parent.path().join("outside");
        let long = Path::new("/").join("d".repeat(60)).join("e".repeat(60));
        for dir in [
            root.join("run"),
            root.join(long.strip_prefix("/").unwrap()),
            outside.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }

        let _listeners = [
            listen(kind, &root.join("run"), "s.sock"),
            listen(kind, &root.join(long.strip_prefix("/").unwrap()), "s.sock"),
            listen(kind, &outside, "s.sock"),
        ];
        symlink("/run/s.sock", root.join("linked")).unwrap();
        symlink(outside.join("s.sock"), root.join("escaped")).unwrap();
        symlink("../outside/s.sock", root.join("rel_escaped")).unwrap();

        for path in [
            PathBuf::from("/run/s.sock"),
            PathBuf::from("run/s.sock"),
            PathBuf::from("/linked"),
            long.join("s.sock"),
        ] {
            assert!(
                connect(kind, &root, resolver, path.clone()).await,
                "{path:?}"
            );
        }

        for path in [
            outside.join("s.sock"),
            PathBuf::from("/escaped"),
            PathBuf::from("/rel_escaped"),
            PathBuf::from("/../outside/s.sock"),
        ] {
            assert!(
                !connect(kind, &root, resolver, path.clone()).await,
                "{path:?}"
            );
        }
    }
}
