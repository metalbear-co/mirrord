use actix_codec::{AsyncRead, AsyncWrite};
use futures::{SinkExt, StreamExt};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

pub mod container;
pub mod kubernetes;
pub mod runtime;

const CONNECTION_CHANNEL_SIZE: usize = 1000;

/// Creates the task that handles the messaging between layer/agent.
/// It does the encoding/decoding of protocol.
pub fn wrap_raw_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>) {
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::default());

    let (in_tx, mut in_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (out_tx, out_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        // Generally, this loop should not be exited early with any `return` statement.
        // We want the `close` below to happen.
        loop {
            tokio::select! {
                msg = in_rx.recv() => match msg {
                    Some(msg) => {
                        if let Err(error) = codec.send(msg).await {
                            tracing::error!(?error, "wrap_raw_connection -> Failed to send client message");
                            break;
                        }
                    }
                    None => {
                        tracing::trace!("wrap_raw_connection -> No more client messages, disconnecting");
                        break;
                    }
                },

                msg = codec.next() => match msg {
                    Some(Ok(msg)) => {
                        if let Err(error) = out_tx.send(msg).await {
                            tracing::error!(?error, "wrap_raw_connection -> Failed to send agent message");
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::error!(?error, "wrap_raw_connection -> Failed to receive agent message");
                        break;
                    }
                    None => {
                        tracing::trace!("wrap_raw_connection -> No more agent messages, disconnecting");
                        break;
                    }
                },
            }
        }

        let _ = codec.close().await;
    });

    (in_tx, out_rx)
}
