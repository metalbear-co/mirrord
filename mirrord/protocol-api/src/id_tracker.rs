/// Utility struct for tracking remote IDs across [`mirrord_protocol`] reconnects.
///
/// Can be used to ensure that the client sees a consistent set of IDs,
/// even though the server tracks them independently in later connections.
///
/// Consider the following scenario:
/// 1. Client opens a file
/// 2. Server responds with remote fd 0
/// 3. Reconnect happens
/// 4. Client opens a file
/// 5. Server response with remote fd 0
/// 6. Client sees fd conflict
///
/// This util prevents it by maintaining an offset for valid IDs:
/// 1. The offset is subtracted from IDs sent to the server. Underflow means that the ID is no
///    longer valid.
/// 2. The offset is added to IDs sent to the server.
/// 3. When a connection is lost, the offset is adjusted.
#[derive(Debug)]
pub struct IdTracker {
    /// The highest ID we've returned to the client.
    highest_client_facing_id: Option<u64>,
    /// Offset we need to add to every ID we receive from the server.
    ///
    /// All lesser IDs received from the clients are invalid.
    current_offset: u64,
}

impl IdTracker {
    /// Creates a new instance with the given ID offset.
    pub fn new(initial_offset: u64) -> Self {
        Self {
            highest_client_facing_id: None,
            current_offset: initial_offset,
        }
    }

    /// Adjusts ID received from the client before it can be sent to the server.
    ///
    /// If the ID is no longer valid (below our current offset), returns [`None`].
    pub fn map_id_from_client(&self, id: u64) -> Option<u64> {
        id.checked_sub(self.current_offset)
    }

    /// Adjusts ID received from the server before it can be sent to the client.
    ///
    /// # Panic
    ///
    /// Panics if the ID overflows.
    pub fn map_id_from_server(&mut self, id: u64) -> u64 {
        let id = id
            .checked_add(self.current_offset)
            .expect("remote id overflow");
        self.highest_client_facing_id = std::cmp::max(self.highest_client_facing_id, Some(id));
        id
    }

    /// Adjusts the current ID offset after connection loss.
    ///
    /// # Panic
    ///
    /// Panics when the offset overflows.
    pub fn server_connection_lost(&mut self) {
        self.current_offset = self
            .highest_client_facing_id
            .map(|fd| fd.checked_add(1).expect("remote id overflow"))
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod test {
    use crate::id_tracker::IdTracker;

    #[test]
    fn id_tracker_fresh() {
        let mut tracker = IdTracker::new(0);

        assert_eq!(tracker.map_id_from_server(0), 0);
        assert_eq!(tracker.map_id_from_server(1), 1);
        assert_eq!(tracker.map_id_from_client(0), Some(0));
        assert_eq!(tracker.map_id_from_client(1), Some(1));

        tracker.server_connection_lost();

        assert_eq!(tracker.map_id_from_client(0), None);
        assert_eq!(tracker.map_id_from_client(1), None);
        assert_eq!(tracker.map_id_from_server(0), 2);
        assert_eq!(tracker.map_id_from_client(0), None);
        assert_eq!(tracker.map_id_from_client(1), None);
        assert_eq!(tracker.map_id_from_client(2), Some(0));
    }
}
