use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use mirrord_protocol::{api::BincodeMessage, ClientMessage, DaemonMessage};
use tokio_stream::wrappers::ReceiverStream;

type Result<T, E = tonic::Status> = std::result::Result<T, E>;

pub struct ClientMessageStream<S>(pub S);

impl<S> Stream for ClientMessageStream<S>
where
    S: Stream<Item = Result<BincodeMessage>> + Unpin,
{
    type Item = Result<ClientMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        S::poll_next(Pin::new(&mut self.0), cx).map(|opt_result| {
            opt_result.map(|message_res| {
                message_res.and_then(|message| {
                    message.as_bincode().map_err(|err| {
                        tonic::Status::invalid_argument(format!("Unable to decode message {err}"))
                    })
                })
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

pub struct DaemonMessageStream(pub ReceiverStream<DaemonMessage>);

impl Stream for DaemonMessageStream {
    type Item = Result<BincodeMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        ReceiverStream::poll_next(Pin::new(&mut self.0), cx).map(|opt_result| {
            opt_result.map(|message| {
                BincodeMessage::from_bincode(message).map_err(|err| {
                    tonic::Status::invalid_argument(format!("Unable to encode message {err}"))
                })
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
