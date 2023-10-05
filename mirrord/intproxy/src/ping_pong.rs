use std::time::Duration;

use mirrord_protocol::ClientMessage;
use thiserror::Error;
use tokio::time::{self, Interval, MissedTickBehavior};

use crate::task_manager::Task;

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
}

pub struct AgentSentMessage {
    pub pong: bool,
}

/// Handles ping pong mechanism on the proxy side.
/// This mechanism exists to keep the `proxy <-> agent` connection alive
/// when there are no requests from the layer.
pub struct PingPong {
    ticker: Interval,
    awaiting_pong: bool,
}

impl PingPong {
    pub async fn new(frequency: Duration) -> Self {
        let mut ticker = time::interval(frequency);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;

        Self {
            ticker,
            awaiting_pong: false,
        }
    }
}

impl Task for PingPong {
    type Id = &'static str;
    type Error = PingPongError;
    type MessageIn = AgentSentMessage;
    type MessageOut = ClientMessage;

    fn id(&self) -> Self::Id {
        "PING_PONG"
    }

    async fn run(
        mut self,
        messages: &mut crate::task_manager::MessageBus<Self>,
    ) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = messages.recv() => match msg {
                    None => break Ok(()),
                    Some(AgentSentMessage { pong: true }) if self.awaiting_pong => {
                        self.ticker.reset();
                    }
                    Some(AgentSentMessage { pong: true }) => break Err(PingPongError::UnmatchedPong),
                    Some(AgentSentMessage { pong: false }) if self.awaiting_pong => {}
                    Some(AgentSentMessage { pong: false }) => {
                        self.ticker.reset();
                    }
                },

                _ = self.ticker.tick() => if self.awaiting_pong {
                    break Err(PingPongError::PongTimeout)
                } else {
                    messages.send(ClientMessage::Ping).await;
                    self.awaiting_pong = true;
                    self.ticker.reset();
                },
            }
        }
    }
}
