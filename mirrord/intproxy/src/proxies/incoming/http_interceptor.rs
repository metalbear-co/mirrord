//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted HTTP connection.

use std::{io, net::SocketAddr};

use hyper::{StatusCode, Version};
use mirrord_protocol::tcp::{
    HttpRequestFallback, HttpResponse, HttpResponseFallback, InternalHttpBody,
};
use thiserror::Error;
use tokio::net::TcpStream;

use super::{http::HttpConnection, InterceptorMessageOut};
use crate::background_tasks::{BackgroundTask, MessageBus};

/// Errors that can occur when executing [`HttpInterceptor`] as a [`BackgroundTask`].
#[derive(Error, Debug)]
pub enum HttpInterceptorError {
    /// IO failed.
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    /// Hyper failed.
    #[error("hyper failed: {0}")]
    HyperError(#[from] hyper::Error),
    /// The layer closed connection too soon to send a request.
    #[error("connection closed too soon")]
    ConnectionClosedTooSoon(HttpRequestFallback),
    /// Received a request with unsupported HTTP version.
    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),
}

/// Manages a single intercepted HTTP connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`IncomingProxy`](super::IncomingProxy)
/// to manage individual connections.
pub struct HttpInterceptor {
    local_destination: SocketAddr,
    version: Version,
}

impl HttpInterceptor {
    /// Crates a new instance. This instance will send requests to the given `local_destination`.
    pub fn new(local_destination: SocketAddr, version: Version) -> Self {
        Self {
            local_destination,
            version,
        }
    }
}

impl HttpInterceptor {
    /// Prepares an HTTP connection.
    async fn connect_to_application(&self) -> Result<HttpConnection, HttpInterceptorError> {
        let target_stream = TcpStream::connect(self.local_destination).await?;
        HttpConnection::handshake(self.version, target_stream).await
    }

    /// Handles the result of sending an HTTP request.
    /// Returns a new request to be sent or an error.
    async fn handle_response(
        &self,
        request: HttpRequestFallback,
        response: Result<hyper::Response<hyper::body::Incoming>, HttpInterceptorError>,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        match response {
                Err(HttpInterceptorError::HyperError(e)) if e.is_closed() => {
                    tracing::warn!(
                        "Sending request to local application failed with: {e:?}. \
                        Seems like the local application closed the connection too early, so \
                        creating a new connection and trying again."
                    );
                    tracing::trace!("The request to be retried: {request:?}.");

                    Err(HttpInterceptorError::ConnectionClosedTooSoon(request))
                }

                Err(HttpInterceptorError::HyperError(e)) if e.is_parse() => {
                    tracing::warn!("Could not parse HTTP response to filtered HTTP request, got error: {e:?}.");
                    let body_message = format!("mirrord: could not parse HTTP response from local application - {e:?}");
                    Ok(HttpResponseFallback::response_from_request(
                        request,
                        StatusCode::BAD_GATEWAY,
                        &body_message,
                    ))
                }

                Err(err) => {
                    tracing::warn!("Request to local application failed with: {err:?}.");
                    let body_message = format!("mirrord tried to forward the request to the local application and got {err:?}");
                    Ok(HttpResponseFallback::response_from_request(
                        request,
                        StatusCode::BAD_GATEWAY,
                        &body_message,
                    ))
                }

                Ok(res) if matches!(request, HttpRequestFallback::Framed(_)) => Ok(
                    HttpResponse::<InternalHttpBody>::from_hyper_response(
                        res,
                        self.local_destination.port(),
                        request.connection_id(),
                        request.request_id()
                    )
                        .await
                        .map(HttpResponseFallback::Framed)
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "Failed to read response to filtered http request: {e:?}. \
                                Please consider reporting this issue on \
                                https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml"
                            );
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            )
                        }),
                ),

                Ok(res) => Ok(
                    HttpResponse::<Vec<u8>>::from_hyper_response(
                        res, self.local_destination.port(),
                        request.connection_id(),
                        request.request_id()
                    )
                        .await
                        .map(HttpResponseFallback::Fallback)
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "Failed to read response to filtered http request: {e:?}. \
                                Please consider reporting this issue on \
                                https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml"
                            );
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            )
                        }),
                ),
            }
    }

    /// Sends the given [`HttpRequestFallback`] to the use application.
    /// Handles retries.
    async fn send_to_user(
        &mut self,
        request: HttpRequestFallback,
        connection: &mut HttpConnection,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        let response = connection.send_request(request.clone()).await;
        let response = self.handle_response(request, response).await;

        // Retry once if the connection was closed.
        if let Err(HttpInterceptorError::ConnectionClosedTooSoon(request)) = response {
            tracing::trace!("Request {request:#?} connection was closed too soon, retrying once!");

            // Create a new connection for this second attempt.
            let new_connection = self.connect_to_application().await?;
            *connection = new_connection;

            let response = connection.send_request(request.clone()).await;
            self.handle_response(request, response).await
        } else {
            response
        }
    }
}

impl BackgroundTask for HttpInterceptor {
    type Error = HttpInterceptorError;
    type MessageIn = HttpRequestFallback;
    type MessageOut = InterceptorMessageOut;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut connection = self.connect_to_application().await?;

        while let Some(request) = message_bus.recv().await {
            let response = self.send_to_user(request, &mut connection).await?;

            message_bus
                .send(InterceptorMessageOut::Http(response))
                .await;
        }

        Ok(())
    }
}
