use std::{num::NonZeroUsize, time::Duration};

/// Configuration for a new [`MirrordClient`](crate::client::MirrordClient).
#[derive(Debug)]
pub struct ClientConfig {
    /// How often should ping messages be sent
    pub ping_pong_interval: Duration,
    /// For long can the client task block when sending a messages to the server.
    ///
    /// Exceeding this timeout will fail the client task.
    pub server_send_timeout: Duration,
    /// How much data can be buffered in a traffic tunnel [`Fifo`](crate::fifo::Fifo).
    ///
    /// If the fifo is full, the task will block when trying to send more data.
    ///
    /// Note that a single tunnel can consist of as many as four fifos:
    /// 1. Inbound HTTP request body frames
    /// 2. Outbound HTTP response body frames
    /// 3. Inbound HTTP upgrade data
    /// 4. Outbound HTTP upgrade data
    pub tunnel_fifo_capacity: NonZeroUsize,
    /// For long can the client task block when sending data to a full traffic tunnel
    /// [`Fifo`](crate::fifo::Fifo).
    ///
    /// Exceeding this timeout will forcefully close the tunnel.
    pub tunnel_fifo_timeout: Duration,
    /// How much data can be buffered in a single port subscription [`Fifo`](crate::fifo::Fifo).
    ///
    /// If the fifo is full, the task will block when trying to send new incoming traffic.
    pub subscription_fifo_capacity: NonZeroUsize,
    /// For long can the client task block when sending new incoming traffic to a full port
    /// subscription [`Fifo`](crate::fifo::Fifo).
    ///
    /// Exceeding this timeout will drop the traffic.
    pub subscription_fifo_timeout: Duration,
    /// How much data can be buffered in a single logs subscription [`Fifo`](crate::fifo::Fifo).
    ///
    /// If the fifo is full, the task will block when trying to send logs from the server.
    pub logs_fifo_capacity: NonZeroUsize,
    /// For long can the client task block when sending a server log to a full logs subscription
    /// [`Fifo`](crate::fifo::Fifo).
    ///
    /// Exceeding this timeout will drop the log.
    pub logs_fifo_timeout: Duration,
    /// Initial offset for the [`IdTracker`](crate::id_tracker::IdTracker) used to manage remote
    /// file descriptors.
    pub initial_fd_offset: u64,
}

impl ClientConfig {
    /// Creates a new default config for the mirrord CLI.
    pub fn cli() -> Self {
        Self {
            ping_pong_interval: Duration::from_secs(30),
            server_send_timeout: Duration::from_secs(30),
            tunnel_fifo_capacity: NonZeroUsize::new(256 * 1024).unwrap(),
            tunnel_fifo_timeout: Duration::from_secs(15),
            subscription_fifo_capacity: NonZeroUsize::new(32 * 1024).unwrap(),
            subscription_fifo_timeout: Duration::from_secs(5),
            logs_fifo_capacity: NonZeroUsize::new(8 * 1024).unwrap(),
            logs_fifo_timeout: Duration::from_secs(1),
            initial_fd_offset: 0,
        }
    }
}
