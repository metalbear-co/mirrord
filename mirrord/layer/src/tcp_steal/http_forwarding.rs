use mirrord_protocol::tcp::{HttpRequestFallback, HttpResponseFallback};
use thiserror::Error;
use tokio::sync::mpsc::error::SendError;

#[derive(Error, Debug)]
pub(crate) enum HttpForwarderError {
    #[error("HTTP Forwarder: Failed to send connection id for closing with error: {0}.")]
    ConnectionCloseSend(#[from] SendError<u64>),

    #[error("HTTP Forwarder: Failed to send http response to main layer task with error: {0}.")]
    ResponseSend(#[from] SendError<HttpResponseFallback>),

    #[error("HTTP Forwarder: Failed to send http request HTTP client task with error: {0}.")]
    Request2ClientSend(#[from] SendError<HttpRequestFallback>),

    #[error("HTTP Forwarder: Could not send http request to application or receive its response with error: {0}.")]
    HttpForwarding(#[from] hyper::Error),

    #[error("HTTP Forwarder: TCP connection failed with error: {0}.")]
    TcpStream(#[from] std::io::Error),

    #[error("HTTP Forwarder: Connection was closed too soon with error: {0:#?}")]
    ConnectionClosedTooSoon(HttpRequestFallback),
}
