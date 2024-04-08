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
    /// Used in [`Self::new`] to check if the connection should be treated as HTTP/2.
    pub const H2_PREFACE: &'static [u8; 14] = b"PRI * HTTP/2.0";

    /// Controls the amount of data we read when trying to detect if the stream's first bytes
    /// contain an HTTP request. Used in [`Self::new`].
    ///
    /// **WARNING**: Can't be too small, otherwise we end up accepting things like "Foo " as valid
    /// HTTP requests.
    pub const MINIMAL_HEADER_SIZE: usize = 10;

    /// Checks if `buffer` contains a prefix of a valid HTTP/1.x request, or if it could be an
    /// HTTP/2 request by comparing it with a slice of [`H2_PREFACE`].
    ///
    /// The given `buffer` must contain at least [`MINIMAL_HEADER_SIZE`] bytes, otherwise this
    /// function always returns [`None`].
    #[tracing::instrument(level = "trace")]
    pub fn new(buffer: &[u8]) -> Option<Self> {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer.len() < Self::MINIMAL_HEADER_SIZE {
            None
        } else if buffer == &Self::H2_PREFACE[..Self::MINIMAL_HEADER_SIZE] {
            Some(Self::V2)
        } else {
            match httparse::Request::new(&mut empty_headers).parse(buffer) {
                Ok(..) | Err(httparse::Error::TooManyHeaders) => Some(Self::V1),
                _ => None,
            }
        }
    }
}
