use std::{io, marker::PhantomData, net::SocketAddr};

use hyper::StatusCode;
use mirrord_protocol::tcp::{
    HttpRequestFallback, HttpResponse, HttpResponseFallback, InternalHttpBody,
};
use thiserror::Error;
use tokio::net::TcpStream;

use super::{http::HttpV, InterceptorMessageOut};
use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    codec::{self, CodecError},
};

#[derive(Error, Debug)]
pub enum HttpInterceptorError {
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    #[error("hyper failed: {0}")]
    HyperError(#[from] hyper::Error),
    #[error("proxy codec failed: {0}")]
    CodecError(#[from] CodecError),
    #[error("connection closed too soon")]
    ConnectionClosedTooSoon(HttpRequestFallback),
}

pub struct HttpInterceptor<V> {
    local_destination: SocketAddr,
    _phantom: PhantomData<fn() -> V>,
}

impl<V> HttpInterceptor<V> {
    pub fn new(local_destination: SocketAddr) -> Self {
        Self {
            local_destination,
            _phantom: Default::default(),
        }
    }
}

impl<V: HttpV> HttpInterceptor<V> {
    async fn connect_to_application(&self) -> Result<V, HttpInterceptorError> {
        let target_stream = self.connect_and_send_source().await?;

        let http_request_sender = V::handshake(target_stream).await?;

        Ok(HttpV::new(http_request_sender))
    }

    async fn connect_and_send_source(&self) -> Result<TcpStream, HttpInterceptorError> {
        let stream = TcpStream::connect(self.local_destination).await?;
        let local_address = stream.local_addr()?;

        let (mut codec_tx, codec_rx) = codec::make_async_framed::<SocketAddr, SocketAddr>(stream);
        codec_tx.send(&local_address).await?;

        let write_half = codec_tx.into_inner();
        let read_half = codec_rx.into_inner();

        Ok(write_half.reunite(read_half).unwrap())
    }

    async fn handle_response(
        &self,
        request: HttpRequestFallback,
        response: Result<hyper::Response<hyper::body::Incoming>, hyper::Error>,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        match response {
                Err(err) if err.is_closed() => {
                    tracing::warn!(
                        "Sending request to local application failed with: {err:?}.
                            Seems like the local application closed the connection too early, so
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
                    HttpResponse::<InternalHttpBody>::from_hyper_response(res, self.local_destination.port(), request.connection_id(), request.request_id())
                        .await
                        .map(HttpResponseFallback::Framed)
                        .unwrap_or_else(|e| {
                            tracing::error!("Failed to read response to filtered http request: {e:?}. \
                            Please consider reporting this issue on \
                            https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml");
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            )
                        }),
                ),

                Ok(res) => Ok(
                    HttpResponse::<Vec<u8>>::from_hyper_response(res, self.local_destination.port(), request.connection_id(), request.request_id())
                        .await
                        .map(HttpResponseFallback::Fallback)
                        .unwrap_or_else(|e| {
                            tracing::error!("Failed to read response to filtered http request: {e:?}. \
                            Please consider reporting this issue on \
                            https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml");
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            )
                        }),
                ),
            }
    }

    async fn send_to_user(
        &mut self,
        request: HttpRequestFallback,
        http_version: &mut V,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        let response = http_version.send_request(request.clone()).await;
        let response = self.handle_response(request, response).await;

        // Retry once if the connection was closed.
        if let Err(HttpInterceptorError::ConnectionClosedTooSoon(request)) = response {
            tracing::trace!("Request {request:#?} connection was closed too soon, retrying once!");

            // Create a new `HttpVersion` handler for this second attempt.
            let new_http_version = self.connect_to_application().await?;
            *http_version = new_http_version;

            let response = http_version.send_request(request.clone()).await;
            self.handle_response(request, response).await
        } else {
            response
        }
    }
}

impl<V: HttpV + Send> BackgroundTask for HttpInterceptor<V> {
    type Error = HttpInterceptorError;
    type MessageIn = HttpRequestFallback;
    type MessageOut = InterceptorMessageOut;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut http_version = self.connect_to_application().await?;

        while let Some(request) = message_bus.recv().await {
            let response = self.send_to_user(request, &mut http_version).await?;

            message_bus
                .send(InterceptorMessageOut::Http(response))
                .await;
        }

        Ok(())
    }
}
