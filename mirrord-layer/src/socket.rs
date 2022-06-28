//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashMap, VecDeque},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::{LazyLock, Mutex, RwLock},
};

use mirrord_protocol::Port;
use socket2::Socket;

use crate::error::LayerError;

pub(crate) mod hooks;
pub(crate) mod ops;

pub(crate) type SocketMap = HashMap<RawFd, MirrorSocket>;

/// TODO(alex) [high] 2022-06-25: 2 ways I see this interacting with agent + dup:
///
/// 1. `Arc<Mutex<MirrorSocket>>` and we keep the `SocketState` idea alive. To make it work just
/// check where `CONNECTION_QUEUE` is being used, and how the agent is working with the socket
/// when we call `socket.listen`;
///
/// 2. Make the thing stateful, have different `HashMap<RawFd, MirrorSocket<State>>` for sockets
/// that are (state) `Bind`, `Listen`, `Connected`.
///
/// Thinking a bit more about it, (2) works, but still requires `Arc`, otherwise we could end up
/// with a dupped socket "A" that is `Connected` for `fd` "5", but is `Bound` for `fd` "6".
///
/// The biggest problem is that we have a structure like:
///
/// ```
///                     Socket (shared state)
///                    /      \
///                   /        \
///                 fd        dup_fd
/// ```
///
/// And `Arc` pretty much solves this problem.
///
/// If I go for (2), then there must be a call that takes **every** dupped socket from the old state
/// `HashMap` into the new one.
pub(crate) static MIRROR_SOCKETS: LazyLock<RwLock<HashMap<RawFd, MirrorSocket>>> =
    LazyLock::new(|| RwLock::new(HashMap::default()));

pub static CONNECTION_QUEUE: LazyLock<Mutex<ConnectionQueue>> =
    LazyLock::new(|| Mutex::new(ConnectionQueue::default()));

/// Struct sent over the socket once created to pass metadata to the hook
#[derive(Debug)]
pub struct SocketInformation {
    pub address: SocketAddr,
}

/// poll_agent loop inserts connection data into this queue, and accept reads it.
#[derive(Debug, Default)]
pub struct ConnectionQueue {
    connections: HashMap<RawFd, VecDeque<SocketInformation>>,
}

impl ConnectionQueue {
    pub fn add(&mut self, fd: &RawFd, info: SocketInformation) {
        self.connections.entry(*fd).or_default().push_back(info);
    }

    pub fn get(&mut self, fd: &RawFd) -> Option<SocketInformation> {
        let mut queue = self.connections.remove(fd)?;
        if let Some(info) = queue.pop_front() {
            if !queue.is_empty() {
                self.connections.insert(*fd, queue);
            }
            Some(info)
        } else {
            None
        }
    }
}

impl SocketInformation {
    pub fn new(address: SocketAddr) -> Self {
        Self { address }
    }
}

trait GetPeerName {
    fn get_peer_name(&self) -> SocketAddr;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    /// Remote address we're connected to
    remote_address: SocketAddr,
    /// Local address it's connected from
    local_address: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    address: SocketAddr,
    mirror_address: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketState {
    Initialized,
    Bound(Bound),
    Listening(Bound),
    Connected(Connected),
}

impl Default for SocketState {
    fn default() -> Self {
        SocketState::Initialized
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MirrorSocket {
    inner: Socket,
    pub state: SocketState,
}

impl MirrorSocket {
    fn get_bound(&self) -> Result<Bound, LayerError> {
        if let SocketState::Bound(bound) = &self.state {
            Ok(*bound)
        } else {
            Err(LayerError::SocketInvalidState)
        }
    }
}

#[inline]
const fn is_ignored_port(port: Port) -> bool {
    port == 0 || (port > 50000 && port < 60000)
}
