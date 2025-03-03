//! Ping pong mechanism implementation on the internal proxy side.
//! This mechanism exists to keep the `proxy <-> agent` connection alive when there are no requests
//! from the layer.
//!
//! Realized using the [`DaemonMessage::Pong`](mirrord_protocol::codec::DaemonMessage::Pong) and
//! [`ClientMessage::Ping`] messages.

use std::time::Duration;

use mirrord_protocol::ClientMessage;
use thiserror::Error;
use tokio::time::{self, Interval, MissedTickBehavior};
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::ConnectionRefresh,
    ProxyMessage,
};

/// Errors that can occur when handling ping pong.
#[derive(Error, Debug)]
pub enum PingPongError {
    /// Agent sent pong but the proxy was not expecting one.
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    /// Agent did not send ping in time.
    #[error("agent did not respond to ping in time")]
    PongTimeout,
}

/// Notification about a [`DeamonMessage::Pong`](mirrord_protocol::DaemonMessage::Pong) received
/// from the agent.
pub enum PingPongMessage {
    AgentSentPong,
    ConnectionRefresh(ConnectionRefresh),
}

/// Encapsulates logic of the ping pong mechanism on the proxy side.
/// Run as a [`BackgroundTask`].
pub struct PingPong {
    /// How often the task should send pings.
    ticker: Interval,
    /// Whether this struct awaits for a pong from the agent.
    awaiting_pong: bool,

    reconnecting: bool,
}

impl PingPong {
    /// Creates a new instance of this struct.
    ///
    /// # Arguments
    ///
    /// * frequency - how often the task should send pings
    pub fn new(frequency: Duration) -> Self {
        let mut ticker = time::interval(frequency);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        Self {
            ticker,
            awaiting_pong: false,
            reconnecting: false,
        }
    }
}

impl BackgroundTask for PingPong {
    type Error = PingPongError;
    type MessageIn = PingPongMessage;
    type MessageOut = ProxyMessage;

    /// Pings the agent with a frequency configured in [`PingPong::new`].
    ///
    /// When the time comes to ping the agent and the previous ping was not answered, this task
    /// exits with an error.
    #[tracing::instrument(level = Level::INFO, name = "ping_pong_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                _ = self.ticker.tick(), if !self.reconnecting => {
                    if self.awaiting_pong {
                        break Err(PingPongError::PongTimeout);
                    } else {
                        tracing::debug!("Sending ping to the agent");
                        let _ = message_bus.send(ProxyMessage::ToAgent(ClientMessage::Ping)).await;
                        self.awaiting_pong = true;
                    }
                },

                msg = message_bus.recv() => match (msg, self.awaiting_pong) {
                    (None, _) => {
                        tracing::debug!("Message bus closed, exiting");
                        break Ok(())
                    },
                    (Some(PingPongMessage::AgentSentPong), true) => {
                        tracing::debug!("Agent responded to ping");
                        self.awaiting_pong = false;
                    },
                    (Some(PingPongMessage::AgentSentPong), false) => {
                        break Err(PingPongError::UnmatchedPong)
                    },
                    (Some(PingPongMessage::ConnectionRefresh(refresh)), _) => {
                        tracing::debug!(info = ?refresh, "Received info about connection refresh");
                        match refresh {
                            ConnectionRefresh::Start => {
                                self.reconnecting = true;
                            }
                            ConnectionRefresh::End => {
                                self.reconnecting = false;
                                self.ticker.reset();
                            }
                        }
                    }

                },
            }
        }
    }
}
