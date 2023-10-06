use std::time::Duration;

use mirrord_protocol::{ClientMessage, DaemonMessage};
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
    time::{self, Interval, MissedTickBehavior},
};

use crate::{error::BackgroundTaskDown, ProxyMessage};

#[derive(Error, Debug)]
pub enum PingPongError {
    #[error("background task panicked")]
    Panic,
    #[error("received an unexpected pong from the agent")]
    UnmatchedPong,
    #[error("agent did not respond to ping in time")]
    PongTimeout,
    #[error("a ping message was sent while already waiting for a pong")]
    DuplicatePing,
}

struct AgentSentMessage {
    pong: bool,
}

struct BackgroundTask {
    ticker: Interval,
    awaiting_pong: bool,
    tx: Sender<ProxyMessage>,
    rx: Receiver<AgentSentMessage>,
}

impl BackgroundTask {
    async fn run(mut self) -> Result<(), PingPongError> {
        self.ticker.tick().await;

        loop {
            tokio::select! {
                _ = self.tx.closed() => break Ok(()),

                _ = self.ticker.tick() => {
                    if self.awaiting_pong {
                        break Err(PingPongError::PongTimeout);
                    } else {
                        let _ = self.tx.send(ProxyMessage::ToAgent(ClientMessage::Ping)).await;
                        self.ticker.reset();
                        self.awaiting_pong = true;
                    }
                },

                msg = self.rx.recv() => match (msg, self.awaiting_pong) {
                    (None, _) => break Ok(()),
                    (Some(AgentSentMessage { pong: true }), true) => {
                        self.awaiting_pong = false;
                        self.ticker.reset();
                    },
                    (Some(AgentSentMessage { pong: false }), true) => {},
                    (Some(AgentSentMessage { pong: true }), false) => break Err(PingPongError::UnmatchedPong),
                    (Some(AgentSentMessage { pong: false }), false) => {
                        self.ticker.reset();
                    }
                },
            }
        }
    }
}

/// Handles ping pong mechanism on the proxy side.
/// This mechanism exists to keep the `proxy <-> agent` connection alive
/// when there are no requests from the layer.
pub struct PingPong {
    task: JoinHandle<Result<(), PingPongError>>,
    tx: Sender<AgentSentMessage>,
}

impl PingPong {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(frequency: Duration, tx: Sender<ProxyMessage>) -> Self {
        let mut ticker = time::interval(frequency);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let (task_tx, task_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let task = BackgroundTask {
            ticker,
            awaiting_pong: false,
            tx,
            rx: task_rx,
        };

        let task = tokio::spawn(task.run());

        Self { task, tx: task_tx }
    }

    pub async fn handle_agent_message(
        &mut self,
        message: &DaemonMessage,
    ) -> Result<(), BackgroundTaskDown> {
        self.tx
            .send(AgentSentMessage {
                pong: matches!(message, DaemonMessage::Pong),
            })
            .await
            .map_err(|_| BackgroundTaskDown)
    }

    pub async fn result(self) -> Result<(), PingPongError> {
        self.task.await.map_err(|_| PingPongError::Panic)?
    }
}
