use std::{io, ops::Not, time::Duration};

use bytes::{Bytes, BytesMut};
use http::Response;
use http_body_util::combinators::BoxBody;
use httparse::Status;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    time::Instant,
};
use tracing::Level;

use crate::util::rolledback_stream::RolledBackStream;

pub mod body;
pub mod error;
pub mod extract_requests;
pub mod filter;
pub mod sender;

/// When the corresponding config flag is enabled, a header with this
/// name is injected into http responses. See
/// `mirrord_config::agent::AgentConfig::inject_headers` for details.
pub(crate) const MIRRORD_AGENT_HTTP_HEADER_NAME: &str = "Mirrord-Agent";

/// [`Response`] type with a boxed body.
pub type BoxResponse = Response<BoxBody<Bytes, hyper::Error>>;

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

        // We parse only the first line of the request,
        // so we don't have to worry about header edge cases.
        let buffer = buffer
            .split_inclusive(|b| *b == b'\n')
            .next()
            .unwrap_or(buffer);
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];
        let mut request = httparse::Request::new(&mut empty_headers);
        let result = httparse::ParserConfig::default()
            .allow_multiple_spaces_in_request_line_delimiters(true)
            .parse_request(&mut request, buffer);

        match result {
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
/// connection or not.
///
/// # Notes
///
/// * The given `timeout` starts elapsing immediately when this function is called, so a stream that
///   never sends any bytes (e.g. a server-first protocol like SMTP) will time out instead of
///   blocking indefinitely. A `timeout` of [`Duration::ZERO`] returns `None` without consuming any
///   bytes.
/// * This function can read arbitrarily large amount of data from the stream. However,
///   [`HttpVersion::detect`] should almost always be able to determine the stream type after
///   reading no more than ~2kb (assuming **very** long request URI).
/// * Consumed data is stored in [`RolledBackStream`]'s prefix, which will be dropped after the data
///   is read again.
pub async fn detect_http_version<IO>(
    mut stream: IO,
    timeout: Duration,
) -> io::Result<(RolledBackStream<IO, BytesMut>, Option<HttpVersion>)>
where
    IO: AsyncRead + Unpin,
{
    let mut buf = BytesMut::with_capacity(1024);
    let mut detected = DetectedHttpVersion::Unknown;
    let timeout_at = Instant::now() + timeout;

    while detected.is_known().not() {
        let result = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(timeout_at) => break,
            result = stream.read_buf(&mut buf) => result,
        };

        let read_size = result?;
        if read_size == 0 {
            break;
        }

        detected = HttpVersion::detect(buf.as_ref());
    }

    Ok((RolledBackStream::new(stream, buf), detected.into_version()))
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use rstest::rstest;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt, duplex},
        time::Instant,
    };

    use super::{DetectedHttpVersion, HttpVersion, detect_http_version};

    #[rstest]
    #[case::known_bug(b"hello ther", DetectedHttpVersion::Unknown)]
    #[case::http2(b"PRI * HTTP/2.0", DetectedHttpVersion::Http(HttpVersion::V2))]
    #[case::http11_full(b"GET / HTTP/1.1\r\n\r\n", DetectedHttpVersion::Http(HttpVersion::V1))]
    #[case::http10_full(b"GET / HTTP/1.0\r\n\r\n", DetectedHttpVersion::Http(HttpVersion::V1))]
    #[case::custom_method(b"FOO / HTTP/1.1\r\n\r\n", DetectedHttpVersion::Http(HttpVersion::V1))]
    #[case::extra_spaces(b"GET / asd d HTTP/1.1\r\n\r\n", DetectedHttpVersion::NotHttp)]
    #[case::bad_version_1(b"GET / HTTP/a\r\n\r\n", DetectedHttpVersion::NotHttp)]
    #[case::bad_version_2(b"GET / HTTP/2\r\n\r\n", DetectedHttpVersion::NotHttp)]
    #[case::multiple_spaces(
        b"GET   /  HTTP/1.1\r\n\r\n",
        DetectedHttpVersion::Http(HttpVersion::V1)
    )]
    #[case::bad_header(
        b"GET / HTTP/1.1\r\n Host: \r\n\r\n",
        DetectedHttpVersion::Http(HttpVersion::V1)
    )]
    #[test]
    fn http_detect(#[case] input: &[u8], #[case] expected: DetectedHttpVersion) {
        let detected = HttpVersion::detect(input);
        assert_eq!(detected, expected,)
    }

    /// `Duration::ZERO` skips detection without consuming any bytes, even when the stream
    /// has data ready to read. Relies on the `biased` `select!` to make the already-elapsed
    /// deadline win deterministically over the ready read.
    #[tokio::test]
    async fn timeout_zero_skips_detection() {
        let (mut server, client) = duplex(64);
        server.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();

        let (mut rolled, version) = detect_http_version(client, Duration::ZERO).await.unwrap();
        assert_eq!(version, None);

        let mut buf = [0; 18];
        rolled.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"GET / HTTP/1.1\r\n\r\n");
    }

    /// The timeout starts elapsing immediately on call, so a server-first protocol that
    /// never sends a byte returns within ~timeout instead of blocking on the first read.
    #[tokio::test]
    async fn timeout_is_connect_relative_for_silent_stream() {
        let (_server, client) = duplex(64);

        let started = Instant::now();
        let (_rolled, version) = detect_http_version(client, Duration::from_millis(100))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(version, None);
        assert!(
            elapsed < Duration::from_millis(900),
            "detect_http_version took {elapsed:?}, expected ~100ms",
        );
    }

    /// A complete HTTP/1.1 request is still detected, and the bytes are preserved in the
    /// returned [`super::RolledBackStream`] so downstream readers see them.
    #[tokio::test]
    async fn detects_http1_request_and_preserves_bytes() {
        let (mut server, client) = duplex(64);
        server.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();

        let (mut rolled, version) = detect_http_version(client, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(version, Some(HttpVersion::V1));

        let mut buf = [0; 18];
        rolled.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"GET / HTTP/1.1\r\n\r\n");
    }
}
