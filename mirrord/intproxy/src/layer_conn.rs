//! Implementation of `layer <-> proxy` connection through a [`TcpStream`].

use futures::StreamExt;
use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, LocalMessage, ProxyToLayerMessage,
    codec::{self, AsyncDecoder, AsyncEncoder, CodecError},
};
use tokio::net::{
    TcpStream,
    tcp::{OwnedReadHalf, OwnedWriteHalf},
};
use tracing::Level;

use crate::{
    ProxyMessage,
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::FromLayer,
};

/// Handles logic of a single `layer <-> proxy` connection.
/// Run as a [`BackgroundTask`].
pub struct LayerConnection {
    layer_codec_tx: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, OwnedWriteHalf>,
    layer_codec_rx: AsyncDecoder<LocalMessage<LayerToProxyMessage>, OwnedReadHalf>,
    layer_id: LayerId,
}

impl LayerConnection {
    /// Wraps a raw [`TcpStream`] to be used as a `layer <-> proxy` connection.
    pub fn new(stream: TcpStream, layer_id: LayerId) -> Self {
        let (layer_codec_tx, layer_codec_rx) = codec::make_async_framed(stream);

        Self {
            layer_codec_rx,
            layer_codec_tx,
            layer_id,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::TRACE))]
    async fn send_and_flush(
        &mut self,
        msg: &LocalMessage<ProxyToLayerMessage>,
    ) -> Result<(), CodecError> {
        self.layer_codec_tx.send(msg).await?;
        self.layer_codec_tx.flush().await
    }
}

impl BackgroundTask for LayerConnection {
    type Error = CodecError;
    type MessageIn = LocalMessage<ProxyToLayerMessage>;
    type MessageOut = ProxyMessage;

    #[tracing::instrument(
        level = Level::INFO, name = "layer_connection_main_loop",
        skip_all, fields(layer_id = ?self.layer_id),
        ret, err,
    )]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), CodecError> {
        loop {
            tokio::select! {
                // `StreamExt::next` is cancel safe, which matters here: this branch loses the race
                // to the one below often enough that a decoder holding partial reads on the stack
                // would desynchronize the stream. See `AsyncDecoder::poll_receive`.
                res = self.layer_codec_rx.next() => match res {
                    Some(Err(e)) => {
                        break Err(e);
                    },
                    None => {
                        tracing::debug!("Layer closed connection, exiting");
                        break Ok(());
                    }
                    Some(Ok(msg)) => message_bus.send(FromLayer { message: msg.inner, message_id: msg.message_id, layer_id: self.layer_id }).await,
                },

                msg = message_bus.recv() => match msg {
                    Some(msg) => self.send_and_flush(&msg).await?,
                    None => {
                        tracing::debug!("Message bus closed, exiting");
                        break Ok(());
                    },
                },
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use futures::{FutureExt, StreamExt};
    use mirrord_intproxy_protocol::codec::{AsyncDecoder, AsyncEncoder};
    use tokio::io::AsyncWriteExt;

    /// Length of the codec's message prefix, see [`mirrord_intproxy_protocol::codec`].
    const PREFIX_BYTES: usize = 4;

    /// [`LayerConnection::run`] selects over the decoder, so that future is dropped every time the
    /// other branch wins the race. If the decoder kept its progress on the stack, the bytes it had
    /// already taken from the socket would vanish with the dropped future, and the next poll would
    /// read the middle of a message as a length prefix.
    ///
    /// A layer opening many outgoing connections at once keeps both directions busy and hits this
    /// within a few hundred messages.
    ///
    /// Cancelling through [`StreamExt::next`] and resuming through `receive` also pins down that
    /// both entry points drive the same state machine, rather than each keeping its own.
    #[tokio::test]
    async fn decoder_resumes_after_being_cancelled_mid_message() {
        let (mut layer, proxy) = tokio::io::duplex(1024);
        let mut decoder: AsyncDecoder<u64, _> = AsyncDecoder::new(proxy);

        let mut frame = Vec::new();
        let mut encoder: AsyncEncoder<u64, _> = AsyncEncoder::new(&mut frame);
        encoder.send(&1234).await.unwrap();
        encoder.flush().await.unwrap();

        let (prefix, payload) = frame.split_at(PREFIX_BYTES);

        // Deliver only the length prefix, then cancel a poll that consumed it, the way `select!`
        // does when the outgoing branch wins.
        layer.write_all(prefix).await.unwrap();
        assert!(
            decoder.next().now_or_never().is_none(),
            "decoder should be pending while the payload is missing"
        );

        // The payload arrives. The next poll has to resume the message in flight rather than treat
        // these bytes as a fresh length prefix.
        //
        // The timeout is what makes a regression fail instead of hang: reading the payload as a
        // prefix yields a huge length, and the decoder then waits forever for bytes that the
        // layer is never going to send.
        layer.write_all(payload).await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(5), decoder.receive())
            .await
            .expect("decoder lost the bytes it consumed before being cancelled")
            .unwrap();
        assert_eq!(received, Some(1234));
    }

    /// The decoder's buffer only grows, so it stays sized for the largest message seen so far. A
    /// shorter message that follows a longer one has to be read and decoded against its own
    /// length, not against whatever the buffer happens to be holding.
    #[tokio::test]
    async fn decoder_reuses_its_buffer_across_message_sizes() {
        let (mut layer, proxy) = tokio::io::duplex(4096);
        let mut decoder: AsyncDecoder<String, _> = AsyncDecoder::new(proxy);

        let long = "x".repeat(512);
        let short = "y".to_owned();

        let mut frames = Vec::new();
        let mut encoder: AsyncEncoder<String, _> = AsyncEncoder::new(&mut frames);
        encoder.send(&long).await.unwrap();
        encoder.send(&short).await.unwrap();
        encoder.flush().await.unwrap();

        layer.write_all(&frames).await.unwrap();

        assert_eq!(decoder.receive().await.unwrap(), Some(long));
        assert_eq!(decoder.receive().await.unwrap(), Some(short));
    }
}
