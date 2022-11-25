use core::{convert::Infallible, mem::MaybeUninit};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use hyper::{body, server::conn::http1, service::service_fn, Request, Response};
use thiserror::Error;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt, BufStream},
    net::{TcpListener, TcpStream},
    sync::mpsc::channel,
};

#[derive(Error, Debug)]
pub enum HttpError {
    #[error("Failed parsing HTTP with 0 bytes!")]
    Empty,

    #[error("Failed parsing HTTP smaller than minimal!")]
    TooSmall,

    #[error("Failed with IO `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("Failed with Parse `{0}`!")]
    Parse(#[from] httparse::Error),
}

struct HttpProxy {}

const MINIMAL_HTTP1_REQUEST: &str = "GET / HTTP/1.1";

/// Checks if the length of a request is of a valid size.
const fn valid_http1_length(length: usize) -> Result<(), HttpError> {
    // TODO(alex): `length == 0` should also be checked in HTTP/2.
    if length == 0 {
        Err(HttpError::Empty)
    } else if length < MINIMAL_HTTP1_REQUEST.len() {
        Err(HttpError::TooSmall)
    } else {
        Ok(())
    }
}

/// Checks if `bytes` contains a _mostly_ valid HTTP/1 request.
#[tracing::instrument(level = "debug", fields(length = %bytes.len()))]
fn valid_http1_request(bytes: &[u8]) -> Result<(), HttpError> {
    use httparse::*;

    valid_http1_length(bytes.len()).and_then(|()| {
        match Request::new(&mut [EMPTY_HEADER; 0]).parse(&bytes[..]) {
            // Ignore error that occurs due to having more headers than the amount allocated.
            Ok(_) | Err(Error::TooManyHeaders) => Ok(()),
            Err(fail) => Err(fail.into()),
        }
    })?;

    Ok(())
}

// TODO(alex) [mid] 2022-11-25: To deal with regex intersection checking
// (avoid 2 users intercepting the same requests?)
// see https://users.rust-lang.org/t/detect-regex-conflict/57184/13
#[tracing::instrument(level = "debug", fields(length = %bytes.len()))]
pub async fn hyper_debug(bytes: &[u8]) -> Result<(), HttpError> {
    valid_http1_request(bytes)?;

    let (mut client, server) = duplex(12345);

    client.write(bytes).await.unwrap();

    let foo = tokio::task::spawn(async move {
        let wat = http1::Builder::new()
            .serve_connection(
                server,
                service_fn(|request: Request<body::Incoming>| async move {
                    // TODO(alex) [high] 2022-11-25: Inspect the request, if it should be captured,
                    // then return it in some wrapper type that indicates this.
                    // Otherwise, insert the request into the response body, then extract it from
                    // the `client.body` (valid for both, as we don't want responses, only
                    // requests).
                    println!("foo");
                    Ok::<_, Infallible>(Response::new("hello".to_string()))
                }),
            )
            .await
            .unwrap();
    })
    .await;

    let mut client_buffer = vec![0; 12345];
    let amount = client.read(&mut client_buffer).await.unwrap();
    println!(
        "client {:#?} amount {:#?}",
        String::from_utf8_lossy(&client_buffer[..amount]),
        amount
    );

    println!("foo {foo:#?}");

    Ok(todo!())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTTP1_SAMPLE: &str =
        "GET / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\n\r\n";

    const HTTP1_BIG_REQUEST: &str = "POST / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\nContent-Length: 1975\r\nContent-Type: application/x-www-form-urlencoded\r\nExpect: 100-continue\r\n\r\n";

    #[tokio::test]
    async fn traffic_hyper() {
        hyper_debug(HTTP1_BIG_REQUEST.as_bytes()).await.unwrap();
    }
}
