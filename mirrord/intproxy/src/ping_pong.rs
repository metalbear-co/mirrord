use std::time::Duration;

use mirrord_protocol::DaemonMessage;
use thiserror::Error;
use tokio::time::{self, Interval, MissedTickBehavior};

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
}

pub type Result<T> = core::result::Result<T, PingPongError>;

/// Handles ping pong mechanism on the proxy side.
/// This mechanism exists to keep the `proxy <-> agent` connection alive
/// when there are no requests from the layer.
pub struct PingPong {
    interval: Interval,
    awaiting_pong: bool,
}

impl PingPong {
    pub async fn new(frequency: Duration) -> Self {
        let mut interval = time::interval(frequency);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;

        Self {
            interval,
            awaiting_pong: false,
        }
    }

    /// Returns when its time to send
    /// [`ClientMessage::Ping`](mirrord_protocol::codec::ClientMessage::Ping) to the agent.
    ///
    /// Returns [`Err`] if this time has come but the agent did not respond to the last ping.
    pub async fn ready(&mut self) -> Result<()> {
        self.interval.tick().await;

        if self.awaiting_pong {
            Err(PingPongError::PongTimeout)
        } else {
            Ok(())
        }
    }

    /// Notifies this struct that a ping message was sent.
    pub fn ping_sent(&mut self) {
        self.interval.reset();
        self.awaiting_pong = true;
    }

    /// Allows this struct to monitor messages coming from the agent.
    /// Should be called for every incoming agent message.
    pub fn inspect_agent_message(&mut self, message: &DaemonMessage) -> Result<()> {
        match (self.awaiting_pong, message) {
            (false, DaemonMessage::Pong) => {
                // Agent sent a pong, but we are not waiting for one.
                Err(PingPongError::UnmatchedPong)
            }
            (true, DaemonMessage::Pong) => {
                // Agent responded to pong.
                self.interval.reset();
                self.awaiting_pong = false;

                Ok(())
            }
            (false, _) => {
                // Agent sent some other message, no need to ping.
                self.interval.reset();

                Ok(())
            }
            (true, _) => {
                // Agent sent some other message, but we are still waiting for the pong.
                // If it is not received in time, something is not right.
                Ok(())
            }
        }
    }
}
