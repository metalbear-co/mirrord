//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted HTTP connection.

use std::{io, marker::PhantomData, net::SocketAddr};

use hyper::StatusCode;
use mirrord_protocol::tcp::{
    HttpRequestFallback, HttpResponse, HttpResponseFallback, InternalHttpBody,
};
use thiserror::Error;
use tokio::net::TcpStream;

use super::{http::HttpConnector, InterceptorMessageOut};
use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    codec::{AsyncEncoder, CodecError},
};

/// Errors that can occur when executing [`HttpInterceptor`] as a [`BackgroundTask`].
#[derive(Error, Debug)]
pub enum HttpInterceptorError {
    /// IO failed.
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    /// Hyper failed.
    #[error("hyper failed: {0}")]
    HyperError(#[from] hyper::Error),
    /// [`codec`](crate::codec) failed.
    #[error("proxy codec failed: {0}")]
    CodecError(#[from] CodecError),
    /// The layer closed connection too soon to send a request.
    #[error("connection closed too soon")]
    ConnectionClosedTooSoon(HttpRequestFallback),
}

/// Manages a single intercepted HTTP connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`IncomingProxy`](super::IncomingProxy)
/// to manage individual connections.
pub struct HttpInterceptor<V> {
    local_destination: SocketAddr,
    _phantom: PhantomData<fn() -> V>,
}

impl<V> HttpInterceptor<V> {
    /// Crates a new instance. This instance will send requests to the given `local_destination`.
    pub fn new(local_destination: SocketAddr) -> Self {
        Self {
            local_destination,
            _phantom: Default::default(),
        }
    }
}

impl<V: HttpConnector> HttpInterceptor<V> {
    /// Prepares an HTTP connection.
    async fn connect_to_application(&self) -> Result<V, HttpInterceptorError> {
        let target_stream = self.connect_and_send_source().await?;
        V::handshake(target_stream).await.map_err(Into::into)
    }

    /// Connects to the local listener and sends it encoded address of the remote peer.
    async fn connect_and_send_source(&self) -> Result<TcpStream, HttpInterceptorError> {
        let stream = TcpStream::connect(self.local_destination).await?;
        let local_address = stream.local_addr()?;

        let mut codec_tx: AsyncEncoder<SocketAddr, TcpStream> = AsyncEncoder::new(stream);
        codec_tx.send(&local_address).await?;
        codec_tx.flush().await?;

        Ok(codec_tx.into_inner())
    }

    /// Handles the result of sending an HTTP request.
    /// Returns a new request to be sent or an error.
    async fn handle_response(
        &self,
        request: HttpRequestFallback,
        response: Result<hyper::Response<hyper::body::Incoming>, hyper::Error>,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        match response {
                Err(err) if err.is_closed() => {
                    tracing::warn!(
                        "Sending request to local application failed with: {err:?}. \
                        Seems like the local application closed the connection too early, so \
                        creating a new connection and trying again."
                    );
                    tracing::trace!("The request to be retried: {request:?}.");

                    Err(HttpInterceptorError::ConnectionClosedTooSoon(request))
                }

                Err(err) if err.is_parse() => {
                    tracing::warn!("Could not parse HTTP response to filtered HTTP request, got error: {err:?}.");
                    let body_message = format!("mirrord: could not parse HTTP response from local application - {err:?}");
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
        connector: &mut V,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        let response = connector.send_request(request.clone()).await;
        let response = self.handle_response(request, response).await;

        // Retry once if the connection was closed.
        if let Err(HttpInterceptorError::ConnectionClosedTooSoon(request)) = response {
            tracing::trace!("Request {request:#?} connection was closed too soon, retrying once!");

            // Create a new connector for this second attempt.
            let new_connector = self.connect_to_application().await?;
            *connector = new_connector;

            let response = connector.send_request(request.clone()).await;
            self.handle_response(request, response).await
        } else {
            response
        }
    }
}

impl<V: HttpConnector + Send> BackgroundTask for HttpInterceptor<V> {
    type Error = HttpInterceptorError;
    type MessageIn = HttpRequestFallback;
    type MessageOut = InterceptorMessageOut;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut connector = self.connect_to_application().await?;

        while let Some(request) = message_bus.recv().await {
            let response = self.send_to_user(request, &mut connector).await?;

            message_bus
                .send(InterceptorMessageOut::Http(response))
                .await;
        }

        Ok(())
    }
}
