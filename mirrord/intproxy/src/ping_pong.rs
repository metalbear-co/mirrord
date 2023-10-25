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

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
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

/// Notification about a message received from the agent.
pub struct AgentMessageNotification {
    /// Whether the message was
    /// [`DaemonMessage::Pong`](mirrord_protocol::codec::DaemonMessage::Pong).
    pub pong: bool,
}

/// Encapsulates logic of the ping pong mechanism on the proxy side.
/// Run as a [`BackgroundTask`].
pub struct PingPong {
    /// How long can the `proxy <-> agent` connection remain silent.
    ticker: Interval,
    /// Whether this struct awaits for a pong from the agent.
    awaiting_pong: bool,
}

impl PingPong {
    /// Creates a new instance of this struct.
    ///
    /// # Arguments
    ///
    /// * frequency - how long can the `proxy <-> agent` connection remain silent
    pub fn new(frequency: Duration) -> Self {
        let mut ticker = time::interval(frequency);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        Self {
            ticker,
            awaiting_pong: false,
        }
    }
}

impl BackgroundTask for PingPong {
    type Error = PingPongError;
    type MessageIn = AgentMessageNotification;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                _ = self.ticker.tick() => {
                    if self.awaiting_pong {
                        tracing::error!("pong timeout");
                        break Err(PingPongError::PongTimeout);
                    } else {
                        tracing::trace!("sending ping");
                        let _ = message_bus.send(ProxyMessage::ToAgent(ClientMessage::Ping)).await;
                        self.ticker.reset();
                        self.awaiting_pong = true;
                    }
                },

                msg = message_bus.recv() => match (msg, self.awaiting_pong) {
                    (None, _) => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(())
                    },
                    (Some(AgentMessageNotification { pong: true }), true) => {
                        tracing::trace!("agent responded to ping");
                        self.awaiting_pong = false;
                        self.ticker.reset();
                    },
                    (Some(AgentMessageNotification { pong: false }), true) => {
                        tracing::trace!("agent sent message, still waiting for pong")
                    },
                    (Some(AgentMessageNotification { pong: true }), false) => {
                        tracing::error!("agent sent an unexpected pong");
                        break Err(PingPongError::UnmatchedPong)
                    },
                    (Some(AgentMessageNotification { pong: false }), false) => {
                        self.ticker.reset();
                    }
                },
            }
        }
    }
}
