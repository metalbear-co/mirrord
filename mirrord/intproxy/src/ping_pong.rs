use std::time::{Duration, Instant};

use mirrord_protocol::ClientMessage;
use thiserror::Error;
use tokio::time::{self, Interval, MissedTickBehavior};
use tokio_util::sync::DropGuard;

use crate::{
    agent_conn::AgentSender,
    system::{Component, ComponentResult, Producer, ComponentRef, ComponentError, WeakComponentRef},
    error::{Result, IntProxyError},
};

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
}

pub enum PingPongMessage {
    AgentSentMessage {
        is_pong: bool,
    },
    Tick {
        requested_at: Instant,
    }
}

/// Handles ping pong mechanism on the proxy side.
/// This mechanism exists to keep the `proxy <-> agent` connection alive
/// when there are no requests from the layer.
pub struct PingPong {
    self_ref: WeakComponentRef<Self>,
    agent_sender: AgentSender,
    awaiting_pong: bool,
    frequency: Duration,
    last_ping_request: DropGuard,
}

impl Component for PingPong {
    type Message = PingPongMessage;
    type Id = &'static str;

    fn id(&self) -> Self::Id {
        "PING_PONG"
    }

    async fn handle(&mut self, message: Self::Message) -> Result<()> {
        match message {
            PingPongMessage::Tick { requested_at } if requested_at < self.last_ping_request => Ok(()),
            PingPongMessage::Tick { .. } if self.awaiting_pong => Err(IntProxyError::PingPong(PingPongError::PongTimeout)),
            PingPongMessage::Tick { .. } => {
                let Some(strong) = self.self_ref.upgrade() else {
                    return Ok(());
                };

                self.agent_sender.send(ClientMessage::Ping).await?;
                self.awaiting_pong = true;

                self.last_ping_request = Instant::now();
                strong.delay(self.frequency, PingPongMessage::Tick { requested_at: self.last_ping_request });

                Ok(())
            }
            PingPongMessage::AgentSentMessage { is_pong: true } if !self.awaiting_pong => Err(IntProxyError::PingPong(PingPongError::UnmatchedPong)),
            PingPongMessage::AgentSentMessage { is_pong: false } if !self.awaiting_pong => {
                Ok(())
            }
            PingPongMessage::AgentSentMessage { is_pong: true } if self.awaiting_pong => {
                self.awaiting_pong = false;
                Ok(())
            }
            PingPongMessage::AgentSentMessage { .. } => Ok(()),
        }
    }
}

impl PingPong {
    pub async fn new(self_ref: ComponentRef<Self>, frequency: Duration, agent_sender: AgentSender) -> Self {
        let last_ping_request = Instant::now();
        self_ref.clone().delay(frequency, PingPongMessage::Tick { requested_at: last_ping_request });

        Self {
            self_ref: self_ref.downgrade(),
            frequency,
            agent_sender,
            awaiting_pong: false,
            last_ping_request,
        }
    }
}
