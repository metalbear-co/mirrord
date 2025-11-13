use std::{
    fmt::{self, Debug},
    io::Read,
    sync::LazyLock,
    time::Duration,
};

use bytes::Bytes;
use hyper::body::Frame;
use mirrord_agent_env::envs;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::time::error::Elapsed;

pub(crate) trait Framelike {
    fn data_ref(&self) -> Option<&[u8]>;
}

impl Framelike for Frame<Bytes> {
    fn data_ref(&self) -> Option<&[u8]> {
        self.data_ref().map(|v| &**v)
    }
}

impl Framelike for InternalHttpBodyFrame {
    fn data_ref(&self) -> Option<&[u8]> {
        match self {
            InternalHttpBodyFrame::Data(payload) => Some(&payload),
            InternalHttpBodyFrame::Trailers(_) => None,
        }
    }
}

/// Implements [`Read`] for a &[Frame]
pub(crate) struct FramesReader<'a, T> {
    remaining: &'a [T],
    read_until: usize,
}

impl<T> Copy for FramesReader<'_, T> {}
impl<T> Clone for FramesReader<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Framelike> Read for FramesReader<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let frame = loop {
            let [first, rest @ ..] = self.remaining else {
                return Ok(0);
            };

            let Some(frame) = first.data_ref() else {
                return Ok(0);
            };

            if self.read_until == frame.len() {
                self.remaining = rest;
                self.read_until = 0;
            } else {
                assert!(self.read_until < frame.len());
                break frame;
            }
        };

        let until = Ord::min(frame.len(), self.read_until + buf.len());
        let range = self.read_until..until;

        buf.get_mut(..range.len())
            .unwrap()
            .copy_from_slice(frame.get(range.clone()).unwrap());

        self.read_until = until;

        Ok(range.len())
    }
}

pub(crate) static MAX_BODY_BUFFER_SIZE: LazyLock<usize> = LazyLock::new(|| {
    match envs::MAX_BODY_BUFFER_SIZE.try_from_env() {
        Ok(Some(t)) => Some(t as usize),
        Ok(None) => {
            tracing::warn!("{} not set, using default", envs::MAX_BODY_BUFFER_SIZE.name);
            None
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                "failed to parse {}, using default",
                envs::MAX_BODY_BUFFER_SIZE.name
            );
            None
        }
    }
    .unwrap_or(64 * 1024)
});

pub(crate) static MAX_BODY_BUFFER_TIMEOUT: LazyLock<Duration> = LazyLock::new(|| {
    Duration::from_millis(
        match envs::MAX_BODY_BUFFER_TIMEOUT.try_from_env() {
            Ok(Some(t)) => Some(t),
            Ok(None) => {
                tracing::warn!(
                    "{} not set, using default",
                    envs::MAX_BODY_BUFFER_TIMEOUT.name
                );
                None
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "failed to parse {}, using default",
                    envs::MAX_BODY_BUFFER_TIMEOUT.name
                );
                None
            }
        }
        .unwrap_or(1000)
        .into(),
    )
});

#[derive(thiserror::Error, Debug)]
pub(crate) enum BufferBodyError {
    #[error("io error while receiving http body: {0}")]
    Hyper(#[from] hyper::Error),
    #[error("body size exceeded max configured size")]
    BodyTooBig,
    #[error("receiving body took too long")]
    Timeout(#[from] Elapsed),
}

pub(crate) enum BufferedBody<T> {
    Empty,

    // Since we only store data frames and one optional trailer frame
    // at the end, we could save a couple hundred bytes by storing raw
    // `Bytes` objects instead of `Frame<Bytes>`-es, (32 vs 96 bytes
    // on 64bit) and having an optional `first_nondata_frame` field.
    // We go with this approach because it's a little simpler to
    // implement.
    Successful(Vec<T>),
    Failed(Vec<T>),
}

impl<T> Debug for BufferedBody<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "Empty"),
            Self::Successful(buf) => f
                .debug_struct("Full")
                .field("frame_count", &buf.len())
                .finish(),
            Self::Failed(buf) => f
                .debug_struct("Partial")
                .field("frame_count", &buf.len())
                .finish(),
        }
    }
}

impl<T> BufferedBody<T> {
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Returns the buffered data if we have anything, regardless of
    /// whether it's full or partial.
    #[inline]
    pub(crate) fn buffered_data(self) -> Option<Vec<T>> {
        match self {
            BufferedBody::Empty => None,
            BufferedBody::Successful(frames) | BufferedBody::Failed(frames) => Some(frames),
        }
    }
}

impl<T: Framelike> BufferedBody<T> {
    /// If we have an entire successfully buffered body, return a
    /// [`FramesReader`] for it.
    pub(crate) fn reader(&self) -> Option<FramesReader<'_, T>> {
        match self {
            Self::Empty | Self::Failed(_) => None,
            Self::Successful(items) => Some(FramesReader {
                remaining: &items,
                read_until: 0,
            }),
        }
    }
}
