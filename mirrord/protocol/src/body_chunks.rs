use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use hyper::body::{Body, Frame};

pub trait BodyExt<B> {
    fn next_frames(&mut self, no_wait: bool) -> FramesFut<'_, B>;
}

impl<B> BodyExt<B> for B
where
    B: Body,
{
    fn next_frames(&mut self, no_wait: bool) -> FramesFut<'_, B> {
        FramesFut {
            body: self,
            no_wait,
        }
    }
}

pub struct FramesFut<'a, B> {
    body: &'a mut B,
    no_wait: bool,
}

impl<B> Future for FramesFut<'_, B>
where
    B: Body<Data = Bytes, Error = hyper::Error> + Unpin,
{
    type Output = hyper::Result<Frames>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut frames = vec![];

        loop {
            let result = match Pin::new(&mut self.as_mut().body).poll_frame(cx) {
                Poll::Ready(Some(Err(error))) => Poll::Ready(Err(error)),
                Poll::Ready(Some(Ok(frame))) => {
                    frames.push(frame);
                    continue;
                }
                Poll::Ready(None) => Poll::Ready(Ok(Frames {
                    frames,
                    is_last: true,
                })),
                Poll::Pending => {
                    if frames.is_empty() && !self.no_wait {
                        Poll::Pending
                    } else {
                        Poll::Ready(Ok(Frames {
                            frames,
                            is_last: false,
                        }))
                    }
                }
            };

            break result;
        }
    }
}

pub struct Frames {
    pub frames: Vec<Frame<Bytes>>,
    pub is_last: bool,
}
