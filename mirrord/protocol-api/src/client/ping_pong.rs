use std::{fmt, pin::Pin, time::Duration};

use mirrord_protocol::DaemonMessage;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};

use crate::client::error::{TaskError, TaskResult};

/// Tracks ping pong state.
///
/// Ping pong logic:
/// 1. Client sends ping on an interval.
/// 2. When there's an unresponded ping, the server cannot remain silent for longer than the
///    interval period (the server has to keep sending *any* messages).
/// 3. The server cannot send unsolicited pongs.
pub struct PingPong {
    /// How often we send ping messages.
    interval: Interval,
    /// How many pongs we're awaiting.
    awaiting: usize,
    /// Timeout for receiving the next message from the server.
    ///
    /// Stored here to reuse the allocation.
    /// Only used when [`Self::awaiting`] > 0.
    recv_timeout: Pin<Box<Sleep>>,
}

impl PingPong {
    pub fn new(interval: Duration) -> Self {
        let mut interval = tokio::time::interval(interval);
        interval.reset();
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Self {
            interval,
            awaiting: 0,
            recv_timeout: Box::pin(tokio::time::sleep(Duration::ZERO)),
        }
    }

    /// Returns [`Ok`] when it's time to send ping and [`Err`] when the server missed a ping.
    ///
    /// # Panic
    ///
    /// Can panic if called again after returning [`Err`], unless [`Self::server_connection_lost`]
    /// was called first.
    pub async fn tick(&mut self) -> TaskResult<()> {
        tokio::select! {
            _ = &mut self.recv_timeout, if self.awaiting > 0 => Err(TaskError::MissedPing),
            _ = self.interval.tick() => {
                self.awaiting += 1;
                if self.awaiting == 1 {
                    self.recv_timeout.as_mut().reset(Instant::now() + self.interval.period());
                }
                Ok(())
            }
        }
    }

    /// Called for each message received from the server.
    pub fn got_message(&mut self, is_pong: bool) -> TaskResult<()> {
        if is_pong {
            self.awaiting = self
                .awaiting
                .checked_sub(1)
                .ok_or_else(|| TaskError::unexpected_message(&DaemonMessage::Pong))?;
        } else if self.awaiting > 0 {
            self.recv_timeout
                .as_mut()
                .reset(Instant::now() + self.interval.period())
        };

        Ok(())
    }

    /// Resets the ping pong state.
    pub fn server_connection_lost(&mut self) {
        self.awaiting = 0;
        self.interval.reset();
    }
}

impl fmt::Debug for PingPong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PingPong")
            .field("interval", &self.interval.period())
            .field("awaiting", &self.awaiting)
            .field(
                "recv_timeout_in",
                &(self.awaiting > 0).then(|| {
                    self.recv_timeout
                        .deadline()
                        .saturating_duration_since(Instant::now())
                }),
            )
            .finish()
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use crate::client::ping_pong::PingPong;

    /// Verifies [`PingPong`] logic.
    #[tokio::test]
    async fn ping_pong() {
        let mut ping_pong = PingPong::new(Duration::from_millis(50));

        // Send ping, receive pong before timeout.
        ping_pong.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        ping_pong.got_message(true).unwrap();

        // Send ping, receive some other message before timeout.
        ping_pong.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        ping_pong.got_message(false).unwrap();

        // Send ping, receive some other message and both pongs before timeout.
        ping_pong.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        ping_pong.got_message(false).unwrap();
        ping_pong.got_message(true).unwrap();
        ping_pong.got_message(true).unwrap();
        ping_pong.tick().await.unwrap();

        // Reset connection state when waiting for ping.
        tokio::time::sleep(Duration::from_millis(60)).await;
        ping_pong.server_connection_lost();
        ping_pong
            .tick()
            .await
            .expect("connection loss should reset ping pong state");
    }
}
