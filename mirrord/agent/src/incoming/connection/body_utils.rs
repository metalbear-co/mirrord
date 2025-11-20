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

use crate::incoming::ConnError;

pub trait Framelike {
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
            InternalHttpBodyFrame::Data(payload) => Some(payload),
            InternalHttpBodyFrame::Trailers(_) => None,
        }
    }
}

/// Implements [`Read`] for slices of [`Framelike`] objects, like
/// [`InternalHttpBodyFrame`] or [`Frame<Bytes>`]
pub struct FramesReader<'a, T> {
	/// The portion of the slice that we've yet to process
    remaining: &'a [T],

	/// How many bytes we've already read from the *current* frame.
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

pub static MAX_BODY_BUFFER_SIZE: LazyLock<usize> = LazyLock::new(|| {
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

pub static MAX_BODY_BUFFER_TIMEOUT: LazyLock<Duration> = LazyLock::new(|| {
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
pub enum BufferBodyError {
    #[error("io error while receiving http body: {0}")]
    Hyper(#[from] hyper::Error),
    #[error(transparent)]
    Conn(#[from] ConnError),
    #[error("body size exceeded max configured size")]
    BodyTooBig,
    #[error("receiving body took too long")]
    Timeout(#[from] Elapsed),
}

pub enum BufferedBody<T> {
    Empty,
    Full(Vec<T>),
    Partial(Vec<T>),
}

impl<T> Debug for BufferedBody<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "Empty"),
            Self::Full(buf) => f
                .debug_struct("Full")
                .field("frame_count", &buf.len())
                .finish(),
            Self::Partial(buf) => f
                .debug_struct("Partial")
                .field("frame_count", &buf.len())
                .finish(),
        }
    }
}

impl<T> BufferedBody<T> {
    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Returns the buffered data if we have anything, regardless of
    /// whether it's full or partial.
    #[inline]
    pub fn buffered_data(self) -> Option<Vec<T>> {
        match self {
            BufferedBody::Empty => None,
            BufferedBody::Full(frames) | BufferedBody::Partial(frames) => Some(frames),
        }
    }
}

impl<T: Framelike> BufferedBody<T> {
    /// If we have an entire successfully buffered body, return a
    /// [`FramesReader`] for it.
    pub fn reader(&self) -> Option<FramesReader<'_, T>> {
        match self {
            Self::Empty | Self::Partial(_) => None,
            Self::Full(items) => Some(FramesReader {
                remaining: items,
                read_until: 0,
            }),
        }
    }
}
#[cfg(test)]
mod test {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_framelike() {
        let frames = vec![
            data(b"0123456789"),
            empty(),
            empty(),
            data(b"ABCDEFGHIJKLMNO"),
            empty(),
            data(b"VWXYZ"),
            nondata(),
            data(b"NEVER"),
        ];
        let mut reader = FramesReader {
			remaining: &frames,
			read_until: 0,
		};

        // Helper for shorter test code
        fn read_n<R: Read>(r: &mut R, n: usize) -> Vec<u8> {
            let mut buf = vec![0; n];
            let read = r.read(&mut buf).unwrap();
            buf.truncate(read);
            buf
        }

        // Read F1 part by part
        assert_eq!(read_n(&mut reader, 3), b"012");
        assert_eq!(read_n(&mut reader, 4), b"3456");
        assert_eq!(read_n(&mut reader, 3), b"789");

        // Skip multiple empty frames
        assert_eq!(read_n(&mut reader, 5), b"ABCDE");

        // Read across frame boundaries
        // Should get 10 remaining from F2, then 2 from F3 ("V", "W")
        assert_eq!(read_n(&mut reader, 12), b"FGHIJKLMNO");

        // Read remaining from F3
        assert_eq!(read_n(&mut reader, 4), b"VWXY");
        assert_eq!(read_n(&mut reader, 1), b"Z");

        // EOF: we hit the first nondata frame and we stay EOF
        for _ in 0..5 {
			let mut buf = [0u8; 1];
			let eof = reader.read(&mut buf).unwrap();
			assert_eq!(eof, 0);
		}
    }

    // ===== Helper Functions =====

    fn data(data: &[u8]) -> TestFrame {
        TestFrame::Data(data.to_vec())
    }

    fn empty() -> TestFrame {
        TestFrame::Data(vec![])
    }

    fn nondata() -> TestFrame {
        TestFrame::Nondata
    }

    // Mock TestFrame for testing
    #[derive(Clone)]
    enum TestFrame {
        Data(Vec<u8>),
        Nondata,
    }

    impl Framelike for TestFrame {
        fn data_ref(&self) -> Option<&[u8]> {
            match self {
                TestFrame::Data(v) => Some(v),
                TestFrame::Nondata => None,
            }
        }
    }
}
