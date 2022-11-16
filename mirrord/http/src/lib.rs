use thiserror::Error;
use tracing::debug;

pub fn add(left: usize, right: usize) -> usize {
    left + right

    /*
     debug!("data {:#?}", String::from_utf8_lossy(&data.bytes));
                if data.bytes.len() < MINIMAL_HTTP_SIZE {
                    warn!("NOT HTTP/1.1");
                } else {
                    let mut headers = vec![httparse::EMPTY_HEADER; 4];
                    let mut request = httparse::Request::new(&mut headers);
                    debug!("request initial {:#?}", request);

                    let request = request.parse(&data.bytes);
                    debug!("request {:#?}", request);
                    debug!("headers {:#?}", headers);
                }
    */
}

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

    let mut headers = vec![httparse::EMPTY_HEADER; 4];
    let mut request = httparse::Request::new(&mut headers);
    println!("request initial {:#?}", request);

    let request = request.parse(&bytes)?;
    println!("request {:#?}", request);
    println!("headers {:#?}", headers);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTTP1_SAMPLE: &str =
        "GET / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\n\r\n";

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
}
