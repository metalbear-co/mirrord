use actix_codec::{AsyncRead, AsyncWrite};
use futures::{SinkExt, StreamExt};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::sync::mpsc;
use tracing::Level;

pub mod container;
pub mod kubernetes;
pub mod runtime;

const CONNECTION_CHANNEL_SIZE: usize = 1000;

/// Spawns a background [`tokio::task`] that sends and receives [`mirrord_protocol`] messages
/// over the given IO stream.
/// 
/// The task handles the encoding/decoding of the [`mirrord_protocol`].
pub fn wrap_raw_connection<IO>(
    stream: IO,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::default());

    let (in_tx, mut in_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (out_tx, out_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        let span = tracing::span!(Level::TRACE, "mirrord_protocol_connection_wrapper_task");
        let _entered = span.enter();

        // Generally, this loop should not be exited early with any `return` statement.
        // We want the `close` below to happen.
        loop {
            tokio::select! {
                msg = in_rx.recv() => match msg {
                    Some(msg) => {
                        if let Err(error) = codec.send(msg).await {
                            tracing::error!(%error, "Failed to send a client message");
                            break;
                        }
                    }
                    None => {
                        tracing::trace!("No more client messages, disconnecting");
                        break;
                    }
                },

                msg = codec.next() => match msg {
                    Some(Ok(msg)) => {
                        if let Err(error) = out_tx.send(msg).await {
                            tracing::error!(%error, "Failed to send an agent message");
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::error!(%error, "Failed to receive an agent message");
                        break;
                    }
                    None => {
                        tracing::trace!("No more agent messages, disconnecting");
                        break;
                    }
                },
            }
        }

        let _ = codec.close().await;
    });

    (in_tx, out_rx)
}
