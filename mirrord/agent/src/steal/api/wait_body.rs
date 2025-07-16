use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;

use crate::incoming::{ConnError, IncomingStreamItem, StolenHttp};

/// Utility wrapper that allows for `.await`ing for the full body of a stolen HTTP request.
pub struct WaitForFullBody(Option<StolenHttp>);

impl From<StolenHttp> for WaitForFullBody {
    fn from(http: StolenHttp) -> Self {
        Self(Some(http))
    }
}

impl Future for WaitForFullBody {
    type Output = Result<StolenHttp, ConnError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let http = this
            .0
            .as_mut()
            .expect("future already polled to completion");

        match std::task::ready!(Pin::new(&mut http.stream).poll_next(cx)) {
            Some(IncomingStreamItem::Frame(frame)) => {
                http.request_head.body_head.push(frame);
                Poll::Pending
            }
            Some(IncomingStreamItem::NoMoreFrames) => {
                http.request_head.body_finished = true;
                let request = this.0.take().expect("checked above");
                Poll::Ready(Ok(request))
            }
            Some(IncomingStreamItem::Finished(Err(error))) => {
                this.0 = None;
                Poll::Ready(Err(error))
            }
            other => {
                tracing::error!(
                    ?other,
                    "Received an unexpected item from an IncomingStream. \
                    This is a bug, please report it."
                );
                this.0 = None;
                Poll::Ready(Err(ConnError::AgentBug(format!(
                    "connection task dropped the channel before sending the Finished item [{}:{}]",
                    file!(),
                    line!(),
                ))))
            }
        }
    }
}
