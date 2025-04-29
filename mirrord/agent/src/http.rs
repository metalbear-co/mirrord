use std::{io, ops::Not, time::Duration};

use bytes::BytesMut;
use futures::future::OptionFuture;
use httparse::Status;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    time::Instant,
};
use tracing::Level;

use crate::util::rolledback_stream::RolledBackStream;

/// Helper enum for representing HTTP/1.x and HTTP/2, which are handled very differently in some
/// parts of the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HttpVersion {
    /// HTTP/1.X
    V1,

    /// HTTP/2
    V2,
}

impl HttpVersion {
    /// Default start of an HTTP/2 request.
    ///
    /// Used in [`Self::detect`] to check if the connection should be treated as HTTP/2.
    const H2_PREFACE: &'static [u8; 14] = b"PRI * HTTP/2.0";

    /// Checks if the given `buffer` contains a prefix of a valid HTTP/1.x or HTTP/2 request.
    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn detect(buffer: &[u8]) -> DetectedHttpVersion {
        if buffer.starts_with(Self::H2_PREFACE) {
            return DetectedHttpVersion::Http(Self::V2);
        }

        let mut empty_headers = [httparse::EMPTY_HEADER; 0];
        let mut request = httparse::Request::new(&mut empty_headers);
        match request.parse(buffer) {
            Ok(Status::Complete(..)) => DetectedHttpVersion::Http(Self::V1),
            Ok(Status::Partial) => match request.version {
                Some(..) => DetectedHttpVersion::Http(Self::V1),
                // If we haven't read enough bytes to consume the HTTP version,
                // we're not certain yet.
                None => DetectedHttpVersion::Unknown,
            },
            // We use a zero-length header array,
            // so this means we successfully parsed the method, uri and version.
            Err(httparse::Error::TooManyHeaders) => DetectedHttpVersion::Http(Self::V1),
            Err(..) => DetectedHttpVersion::NotHttp,
        }
    }
}

/// Output of HTTP version detection on an prefix of an incoming stream.
#[derive(PartialEq, Eq, Debug)]
pub enum DetectedHttpVersion {
    /// We're certain that the stream is an HTTP connection.
    Http(HttpVersion),
    /// We're not sure yet.
    Unknown,
    /// We're certain that the stream is **not** an HTTP connection.
    NotHttp,
}

impl DetectedHttpVersion {
    /// If the stream is known to be an HTTP connection,
    /// returns its version.
    ///
    /// Otherwise, returns [`None`].
    pub fn into_version(self) -> Option<HttpVersion> {
        match self {
            Self::Http(version) => Some(version),
            Self::Unknown => None,
            Self::NotHttp => None,
        }
    }

    /// Returns whether it's known whether the stream is an HTTP connection or not.
    pub fn is_known(&self) -> bool {
        match self {
            Self::Http(_) => true,
            Self::Unknown => false,
            Self::NotHttp => true,
        }
    }
}

/// Attempts to detect HTTP version from the first bytes of a stream.
///
/// Keeps reading data until the timeout elapses or we're certain whether the stream is an HTTP
/// connection or not. Note that the given `timeout` starts elapsing only after we complete the
/// first read.
pub async fn detect_http_version<IO>(
    mut stream: IO,
    timeout: Duration,
) -> io::Result<(RolledBackStream<IO, BytesMut>, Option<HttpVersion>)>
where
    IO: AsyncRead + Unpin,
{
    let mut buf = BytesMut::with_capacity(1024);
    let mut detected = DetectedHttpVersion::Unknown;
    let mut timeout_at: Option<Instant> = None;

    while detected.is_known().not() {
        let timeout_fut = OptionFuture::from(timeout_at.map(tokio::time::sleep_until));

        let result = tokio::select! {
            Some(..) = timeout_fut => break,
            result = stream.read_buf(&mut buf) => result,
        };

        let read_size = result?;
        if read_size == 0 {
            break;
        }

        timeout_at = timeout_at.or_else(|| Some(Instant::now() + timeout));
        detected = HttpVersion::detect(buf.as_ref());
    }

    Ok((RolledBackStream::new(stream, buf), detected.into_version()))
}

#[cfg(test)]
mod test {
    use rstest::rstest;

    use super::{DetectedHttpVersion, HttpVersion};

    #[rstest]
    #[case::known_bug(b"hello ther", DetectedHttpVersion::Unknown)]
    #[case::http2(b"PRI * HTTP/2.0", DetectedHttpVersion::Http(HttpVersion::V2))]
    #[case::http11_full(b"GET / HTTP/1.1\r\n\r\n", DetectedHttpVersion::Http(HttpVersion::V1))]
    #[case::http10_full(b"GET / HTTP/1.0\r\n\r\n", DetectedHttpVersion::Http(HttpVersion::V1))]
    #[case::custom_method(b"FOO / HTTP/1.1\r\n\r\n", DetectedHttpVersion::Http(HttpVersion::V1))]
    #[case::extra_spaces(b"GET / asd d HTTP/1.1\r\n\r\n", DetectedHttpVersion::NotHttp)]
    #[case::bad_version_1(b"GET / HTTP/a\r\n\r\n", DetectedHttpVersion::NotHttp)]
    #[case::bad_version_2(b"GET / HTTP/2\r\n\r\n", DetectedHttpVersion::NotHttp)]
    #[test]
    fn http_detect(#[case] input: &[u8], #[case] expected: DetectedHttpVersion) {
        let detected = HttpVersion::detect(input);
        assert_eq!(detected, expected,)
    }
}
