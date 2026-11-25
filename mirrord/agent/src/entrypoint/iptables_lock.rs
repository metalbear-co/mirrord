//! Lock that keeps agents of the same target container from running at the same time.

use std::{
    io,
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixDatagram},
    },
};

use tracing::warn;

use crate::error::{AgentError, AgentResult};

/// Lock on the iptables chains of one target container.
///
/// The lock is an abstract Unix socket, bound to a name made from the chain identifier. The kernel
/// keeps abstract socket names per network namespace, and releases the name when the socket
/// closes, also when the process dies. So the lock must be acquired on a thread in the target's
/// network namespace.
///
/// The guard process holds the lock while the agent runs, so a second agent for the same target
/// container does not start.
pub(crate) struct IptablesLock {
    _socket: UnixDatagram,
}

impl IptablesLock {
    /// Acquires the lock for the chain identifier `identifier`.
    ///
    /// Returns [`AgentError::IPTablesLocked`] if another agent holds the lock. On any other error,
    /// logs a warning and returns [`None`], so the agent can work without the lock.
    pub(crate) fn try_acquire(identifier: &str) -> AgentResult<Option<Self>> {
        let name = format!("mirrord-agent-iptables-lock-{identifier}");

        let result = SocketAddr::from_abstract_name(name.as_bytes())
            .and_then(|address| UnixDatagram::bind_addr(&address));

        match result {
            Ok(socket) => Ok(Some(Self { _socket: socket })),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                Err(AgentError::IPTablesLocked)
            }
            Err(error) => {
                warn!(%error, name, "Failed to acquire the iptables lock, continuing without it");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock has one holder at a time, and closing the holder's socket releases it.
    #[test]
    fn lock_has_one_holder_and_is_released_on_drop() {
        let identifier = format!("test-{}", std::process::id());

        let lock = IptablesLock::try_acquire(&identifier)
            .unwrap()
            .expect("the first acquire should succeed");
        assert!(matches!(
            IptablesLock::try_acquire(&identifier),
            Err(AgentError::IPTablesLocked)
        ));

        drop(lock);
        assert!(IptablesLock::try_acquire(&identifier).unwrap().is_some());
    }
}
