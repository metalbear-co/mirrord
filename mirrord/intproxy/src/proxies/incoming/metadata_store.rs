use std::collections::HashMap;

use mirrord_intproxy_protocol::{ConnMetadataRequest, ConnMetadataResponse};
use mirrord_protocol::ConnectionId;

/// Maps local socket address pairs to remote.
///
/// Allows for extracting the original socket addresses of peers of a remote connection.
#[derive(Default)]
pub struct MetadataStore {
    prepared_responses: HashMap<ConnMetadataRequest, ConnMetadataResponse>,
    expected_requests: HashMap<ConnectionId, ConnMetadataRequest>,
}

impl MetadataStore {
    /// Retrieves remote addresses for the given pair of local addresses.
    ///
    /// If the mapping is not found, returns the local addresses unchanged.
    pub fn get(&mut self, req: ConnMetadataRequest) -> ConnMetadataResponse {
        self.prepared_responses
            .remove(&req)
            .unwrap_or_else(|| ConnMetadataResponse {
                remote_source: req.peer_address,
                local_address: req.listener_address.ip(),
            })
    }

    /// Adds a new `req`->`res` mapping to this struct.
    ///
    /// Marks that the mapping is related to the remote connection with the given id.
    pub fn expect(
        &mut self,
        req: ConnMetadataRequest,
        connection: ConnectionId,
        res: ConnMetadataResponse,
    ) {
        self.expected_requests.insert(connection, req.clone());
        self.prepared_responses.insert(req, res);
    }

    /// Clears mapping related to the remote connection with the given id.
    pub fn no_longer_expect(&mut self, connection: ConnectionId) {
        let Some(req) = self.expected_requests.remove(&connection) else {
            return;
        };
        self.prepared_responses.remove(&req);
    }
}
