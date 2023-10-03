use std::time::Duration;

use mirrord_protocol::ClientMessage;
use thiserror::Error;
use tokio::{
    sync::mpsc::Receiver,
    time::{self, Interval, MissedTickBehavior},
};

use crate::{agent_conn::AgentSender, system::Component};

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
}

pub enum PingPongMessage {
    Pong,
    NotPong,
}

/// Handles ping pong mechanism on the proxy side.
/// This mechanism exists to keep the `proxy <-> agent` connection alive
/// when there are no requests from the layer.
pub struct PingPong {
    agent_sender: AgentSender,
    ticker: Interval,
    awaiting_pong: bool,
}

impl Component for PingPong {
    type Message = PingPongMessage;
    type Id = &'static str;
    type Error = PingPongError;

    fn id(&self) -> Self::Id {
        "PING_PONG"
    }

    async fn run(mut self, mut message_rx: Receiver<Self::Message>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                _ = self.ticker.tick() => {
                    if self.awaiting_pong {
                        break Err(PingPongError::UnmatchedPong);
                    } else {
                        self.agent_sender.send(ClientMessage::Ping).await;
                        self.awaiting_pong = true;
                    }
                }
                msg = message_rx.recv() => match msg {
                    None => break Ok(()),
                    Some(PingPongMessage::Pong) if self.awaiting_pong => self.awaiting_pong = false,
                    Some(PingPongMessage::Pong) => break Err(PingPongError::UnmatchedPong),
                    Some(PingPongMessage::NotPong) if self.awaiting_pong => {},
                    Some(PingPongMessage::NotPong) => {
                        self.ticker.reset();
                    }
                }
            }
        }
    }
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
}
