use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};

use mirrord_intproxy_protocol::SocketMetadataResponse;
use mirrord_protocol::outgoing::SocketAddress;

/// Store for remote addresses corresponding to local sockets managed by the internal proxy.
///
/// Can be cloned and shared between tasks.
#[derive(Clone, Debug, Default)]
pub struct LocalSockets(Arc<RwLock<HashMap<SocketAddress, SocketMetadataResponse>>>);

impl LocalSockets {
    /// Inserts remote addresses for the given local address.
    ///
    /// Returns a guard that will remove the entry when dropped.
    pub fn insert(
        &self,
        local_address: SocketAddress,
        response: SocketMetadataResponse,
    ) -> LocalSocketEntry {
        self.0
            .write()
            .expect("local sockets mutex is poisoned")
            .insert(local_address.clone(), response);

        LocalSocketEntry {
            sockets: self.clone(),
            local_address,
        }
    }

    pub fn get(&self, local_address: &SocketAddress) -> Option<SocketMetadataResponse> {
        self.0
            .read()
            .expect("local sockets mutex is poisoned")
            .get(local_address)
            .cloned()
    }
}

/// Guard for an entry in [`LocalSockets`].
///
/// Removes the entry when dropped.
pub struct LocalSocketEntry {
    sockets: LocalSockets,
    local_address: SocketAddress,
}

impl LocalSocketEntry {
    /// Uses the given function to modify the entry in place.
    pub fn modify<F>(&self, f: F)
    where
        F: FnOnce(&mut SocketMetadataResponse),
    {
        self.sockets
            .0
            .write()
            .expect("local sockets mutex is poisoned")
            .get_mut(&self.local_address)
            .map(f);
    }
}

impl fmt::Debug for LocalSocketEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.local_address.fmt(f)
    }
}

impl Drop for LocalSocketEntry {
    fn drop(&mut self) {
        let Ok(mut sockets) = self.sockets.0.write() else {
            return;
        };
        sockets.remove(&self.local_address);
    }
}
