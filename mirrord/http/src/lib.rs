use thiserror::Error;
use tracing::debug;

// TODO(alex) [mid] 2022-11-17: To parse http2, set up a client/server (hyper) so we can grab the
// request, pass it to ourselves (to this dummy hyper thingy) and then "parse" with hyper.

#[derive(Error, Debug)]
pub enum HttpError {
    #[error("Failed parsing HTTP with 0 bytes!")]
    Empty,

    #[error("Failed parsing HTTP smaller than minimal!")]
    TooSmall,

    #[error("Failed parsing HTTP with `{0}`!")]
    HttParse(#[from] httparse::Error),
}

const MINIMAL_HTTP1_REQUEST: &str = "GET / HTTP/1.1";

const fn validate_length(length: usize) -> Result<(), HttpError> {
    if length == 0 {
        Err(HttpError::Empty)
    } else if length < MINIMAL_HTTP1_REQUEST.len() {
        Err(HttpError::TooSmall)
    } else {
        Ok(())
    }
}

pub fn parse(bytes: &[u8]) -> Result<(), HttpError> {
    validate_length(bytes.len())?;

    let mut headers = vec![httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    debug!("request initial {:#?}", request);

    let request = request.parse(&bytes)?;
    debug!("request {:#?}", request);
    debug!("headers {:#?}", headers);

    Ok(())
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

    use super::*;

    const HTTP1_SAMPLE: &str =
        "GET / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\n\r\n";

    const HTTP1_BIG_REQUEST: &str = "POST / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\nContent-Length: 1975\r\nContent-Type: application/x-www-form-urlencoded\r\nExpect: 100-continue\r\n\r\n";

    fn tracing_start() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::ACTIVE)
                    .compact(),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    #[test]
    #[should_panic]
    fn test_panic_empty_buffer() {
        parse("".as_bytes()).unwrap();
    }

    #[test]
    #[should_panic]
    fn test_panic_http1_invalid() {
        parse("GET / HTTP/7.0".as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_http1_minimal() {
        parse(MINIMAL_HTTP1_REQUEST.as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_http1_sample() {
        parse(HTTP1_SAMPLE.as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_http1_big() {
        tracing_start();
        parse(HTTP1_BIG_REQUEST.as_bytes()).unwrap();
    }
}
