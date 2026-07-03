//! [`BackgroundTask`] used by [`OutgoingProxy`](super::OutgoingProxy) to manage a single
//! intercepted connection.

mod delay_queue;
pub(super) mod read_queue;
pub(super) mod write_queue;

use std::{future::poll_fn, io, sync::Arc};

use bytes::Bytes;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::PollSemaphore;
use tracing::Level;

use super::InterceptorId;
use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    proxies::outgoing::net_protocol_ext::{PreparedSocket, READ_BUFFER_BYTES},
};

/// Messages sent from the [`OutgoingProxy`](super::OutgoingProxy) to the [`Interceptor`] via the
/// [`MessageBus`].
///
/// To handle some `ChaosRule`s, that act on ongoing connections (intercepted connections that match
/// a chaos selector), the `OutgoingProxy` has to send some special messages (these commands), where
/// previously it only needed to send [`Bytes`].
pub enum InterceptorCommand {
    /// [`Bytes`] sent from the agent to the [`Interceptor`].
    Data(Bytes),

    /// Shutdown this [`Interceptor`], we have no more messages to process for it (it's a sort of
    /// agent side shutdown, there are no bytes coming from the agent for this `Interceptor`
    /// connection).
    Shutdown,

    /// The [`Interceptor`] matched a `ChaosRule` `ConnectionErrorType::Reset`, so we stop
    /// forwarding the traffic to the agent, and call `ConnectedSocket::reset`.
    Reset,

    /// The [`Interceptor`] matched a `ChaosRule` `ConnectionErrorType::TimedOut`, so we stop
    /// forwarding the traffic to the agent, mark the `Interceptor` as chaos blocked (calling
    /// `OutgoingProxy::abort_agent_write_queue`).
    ///
    /// Socket is open, but no bytes are sent to neither the agent or the layer.
    Stall,
}

/// Maximum amount of data (in bytes) that can be in flight in the client-to-agent direction of a
/// single intercepted connection, i.e. read from the layer socket but not yet sent to the agent.
///
/// Once this budget is exhausted, the [`Interceptor`] stops reading the layer socket, which applies
/// real backpressure to the client app instead of buffering unbounded data in the
/// [`AgentWriteQueue`](write_queue::AgentWriteQueue).
///
/// Must be larger than [`READ_BUFFER_BYTES`], so that a single read chunk always fits in the
/// budget; otherwise acquiring permits for it would block forever.
pub(super) const WRITE_BUDGET_BYTES: usize = READ_BUFFER_BYTES * 8;

/// Manages a single intercepted connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`OutgoingProxy`](super::OutgoingProxy)
/// to manage individual connections.
pub struct Interceptor {
    id: InterceptorId,
    socket: Option<PreparedSocket>,
    /// Budget for in-flight client-to-agent bytes of a intercepted connection.
    write_budget: Arc<Semaphore>,
}

impl Interceptor {
    /// Creates a new instance. This instance will use the provided [`PreparedSocket`] to accept the
    /// layer's connection and manage it.
    pub fn new(id: InterceptorId, socket: PreparedSocket, write_budget: Arc<Semaphore>) -> Self {
        Self {
            id,
            socket: Some(socket),
            write_budget,
        }
    }
}

impl BackgroundTask for Interceptor {
    type Error = io::Error;
    type MessageIn = InterceptorCommand;
    type MessageOut = (Bytes, OwnedSemaphorePermit);

    /// Accepts one connection the owned [`PreparedSocket`] and transparently proxies bytes between
    /// the [`MessageBus`] and the new
    /// [`ConnectedSocket`](crate::proxies::outgoing::net_protocol_ext::ConnectedSocket).
    ///
    /// # Notes
    ///
    /// 1. When the peer shuts down writing, a single 0-sized read is sent through the
    ///    [`MessageBus`]. This is to notify the agent about the shutdown condition.
    ///
    /// 2. Control messages received from the [`MessageBus`] can shut down, reset, or stall the
    ///    connection with the peer.
    ///
    /// 3. This implementation exits only when an error is encountered or the [`MessageBus`] is
    ///    closed.
    ///
    /// 4. Before a chunk read from the layer is forwarded, we acquire one [`Self::write_budget`]
    ///    permit per byte. While a chunk waits for permits we stop reading the layer socket, which
    ///    backpressures the client app instead of buffering unbounded data downstream.
    #[tracing::instrument(
        level = Level::DEBUG,
        name = "outgoing_interceptor_main_loop"
        skip_all, fields(id = %self.id),
        ret, err(level = Level::WARN),
    )]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let Some(socket) = self.socket.take() else {
            return Ok(());
        };

        let mut connected_socket = tokio::select! {
            socket = socket.accept() => socket?,
            _ = message_bus.closed_token().cancelled() => {
                tracing::trace!("Connection closed from the outgoing_proxy side, exiting");
                return Ok(());
            },
        };

        let mut write_budget = PollSemaphore::new(Arc::clone(&self.write_budget));
        let mut pending_write: Option<Bytes> = None;
        let mut reading_closed = false;

        loop {
            tokio::select! {
                // Read the next chunk, but only while we are not already holding one waiting for budget.
                read = connected_socket.receive(), if pending_write.is_none() && !reading_closed => match read {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    },
                    Err(e) => break Err(e),
                    Ok(bytes) => {
                        if bytes.is_empty() {
                            tracing::trace!("Layer shutdown, sending a 0-sized read to inform the agent");
                            reading_closed = true;
                        } else {
                            tracing::trace!(bytes = bytes.len(), "Received data from the layer");
                        }

                        pending_write = Some(Bytes::from(bytes));
                    },
                },

                // Acquire write permit for the pending chunk, then forward it together with
                // the permit. The permit is released downstream once the chunk is sent to agent.
                permit = poll_fn(|cx| {
                    let len = pending_write.as_ref().map_or(0, Bytes::len);
                    write_budget.poll_acquire_many(cx, len.try_into().unwrap_or(READ_BUFFER_BYTES as u32))
                }), if pending_write.is_some() => {
                    let bytes = pending_write.take().expect("Checked that pending write is not none");
                    let Some(permit) = permit else {
                        tracing::trace!("Client-to-agent write budget semaphore closed, exiting");
                        break Ok(());
                    };

                    message_bus.send((bytes, permit)).await
                },

                msg = message_bus.recv() => match msg {
                    Some(InterceptorCommand::Data(bytes)) => {
                        if bytes.is_empty() {
                            tracing::trace!("Agent shutdown, shutting down connection with layer");
                            connected_socket.shutdown().await?;
                        } else {
                            tracing::trace!(bytes = bytes.len(), "Received data from the agent");
                            connected_socket.send(&bytes).await?;
                        }
                    }
                    Some(InterceptorCommand::Shutdown) => {
                        tracing::trace!("Agent shutdown, shutting down connection with layer");
                        connected_socket.shutdown().await?;
                    }
                    Some(InterceptorCommand::Reset) => {
                        tracing::trace!("Resetting connection with layer");
                        connected_socket.reset()?;
                        drop(connected_socket);
                        break Ok(());
                    }
                    Some(InterceptorCommand::Stall) => {
                        tracing::trace!("Stalling connection with layer");
                        message_bus.closed_token().cancelled().await;
                        break Ok(());
                    }

                    None => {
                        tracing::trace!("Connection closed from the outgoing_proxy side, exiting");
                        break Ok(())
                    }
                },
            }
        }
    }
}
