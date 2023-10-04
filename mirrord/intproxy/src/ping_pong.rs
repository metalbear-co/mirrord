use std::time::Duration;

use mirrord_protocol::{ClientMessage, DaemonMessage};
use thiserror::Error;
use tokio::time::{self, Interval, MissedTickBehavior};

use crate::agent_conn::{AgentCommunicationError, AgentSender};

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
    #[error("internal proxy already awaits for agent's pong")]
    AlreadyAwaitingPong,
    #[error("failed to send ping to the agent: {0}")]
    AgentCommunicationError(#[from] AgentCommunicationError),
}

/// Handles ping pong mechanism on the proxy side.
/// This mechanism exists to keep the `proxy <-> agent` connection alive
/// when there are no requests from the layer.
pub struct PingPong {
    agent_sender: AgentSender,
    ticker: Interval,
    awaiting_pong: bool,
}

impl PingPong {
    pub async fn new(frequency: Duration, agent_sender: AgentSender) -> Self {
        let mut ticker = time::interval(frequency);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;

        Self {
            agent_sender,
            ticker,
            awaiting_pong: false,
        }
    }

    pub async fn ready(&mut self) -> Result<(), PingPongError> {
        self.ticker.tick().await;

        if self.awaiting_pong {
            Err(PingPongError::PongTimeout)
        } else {
            Ok(())
        }
    }

    pub async fn send_ping(&mut self) -> Result<(), PingPongError> {
        if self.awaiting_pong {
            Err(PingPongError::AlreadyAwaitingPong)
        } else {
            self.agent_sender.send(ClientMessage::Ping).await?;
            self.awaiting_pong = true;
            Ok(())
        }
    }

    pub fn inspect_agent_message(&mut self, message: &DaemonMessage) -> Result<(), PingPongError> {
        match (message, self.awaiting_pong) {
            (DaemonMessage::Pong, true) => {
                self.awaiting_pong = false;
                self.ticker.reset();
                Ok(())
            }
            (DaemonMessage::Pong, false) => Err(PingPongError::UnmatchedPong),
            (_, true) => Ok(()),
            (_, false) => {
                self.ticker.reset();
                Ok(())
            }
        }
    }
}
