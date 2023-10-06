use std::time::Duration;

use mirrord_protocol::ClientMessage;
use thiserror::Error;
use tokio::time::{self, Interval, MissedTickBehavior};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    ProxyMessage,
};

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
    #[error("a ping message was sent while already waiting for a pong")]
    DuplicatePing,
}

pub struct AgentMessageNotification {
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
                        break Err(PingPongError::PongTimeout);
                    } else {
                        let _ = message_bus.send(ProxyMessage::ToAgent(ClientMessage::Ping)).await;
                        self.ticker.reset();
                        self.awaiting_pong = true;
                    }
                },

                msg = message_bus.recv() => match (msg, self.awaiting_pong) {
                    (None, _) => break Ok(()),
                    (Some(AgentMessageNotification { pong: true }), true) => {
                        self.awaiting_pong = false;
                        self.ticker.reset();
                    },
                    (Some(AgentMessageNotification { pong: false }), true) => {},
                    (Some(AgentMessageNotification { pong: true }), false) => break Err(PingPongError::UnmatchedPong),
                    (Some(AgentMessageNotification { pong: false }), false) => {
                        self.ticker.reset();
                    }
                },
            }
        }
    }
}
