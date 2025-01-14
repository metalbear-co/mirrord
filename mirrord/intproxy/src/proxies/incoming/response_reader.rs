use std::{convert::Infallible, time::Instant};

use http_body_util::BodyExt;
use hyper::body::{Frame, Incoming};
use mirrord_protocol::{
    batched_body::BatchedBody,
    tcp::{
        ChunkedHttpBody, ChunkedHttpError, ChunkedResponse, HttpResponse, InternalHttpBody,
        InternalHttpBodyFrame, LayerTcpSteal,
    },
    ConnectionId, RequestId,
};
use tracing::Level;

use super::http::PeekedBody;
use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    proxies::incoming::http::LocalHttpError,
};

pub enum HttpResponseReader {
    Legacy(HttpResponse<PeekedBody>),
    Framed(HttpResponse<PeekedBody>),
    Chunked {
        connection_id: ConnectionId,
        request_id: RequestId,
        body: Incoming,
    },
}

impl HttpResponseReader {
    fn request_id(&self) -> RequestId {
        match self {
            Self::Legacy(response) => response.request_id,
            Self::Framed(response) => response.request_id,
            Self::Chunked { request_id, .. } => *request_id,
        }
    }

    fn connection_id(&self) -> ConnectionId {
        match self {
            Self::Legacy(response) => response.connection_id,
            Self::Framed(response) => response.connection_id,
            Self::Chunked { connection_id, .. } => *connection_id,
        }
    }
}

impl BackgroundTask for HttpResponseReader {
    type Error = Infallible;
    type MessageIn = Infallible;
    type MessageOut = LayerTcpSteal;

    #[tracing::instrument(
        level = Level::TRACE,
        name = "http_response_reader_main_loop",
        fields(
            connection_id = self.connection_id(),
            request_id = self.request_id(),
        ),
        skip_all, err,
    )]
    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        match self {
            Self::Legacy(mut response) => {
                let tail = match response.internal_response.body.tail.take() {
                    Some(incoming) => {
                        let start = Instant::now();
                        tokio::select! {
                            _ = message_bus.recv() => {
                                tracing::trace!("Message bus closed, exiting");
                                return Ok(());
                            },

                            result = incoming.collect() => match result {
                                Ok(data) => {
                                    tracing::trace!(
                                        elapsed_s = start.elapsed().as_secs_f32(),
                                        "Collected the whole body.",
                                    );
                                    Vec::from(data.to_bytes())
                                },

                                Err(error) => {
                                    tracing::warn!(
                                        connection_id = response.connection_id,
                                        request_id = response.request_id,
                                        %error,
                                        "Failed to read the response body.",
                                    );

                                    let response = LocalHttpError::ReadBodyFailed(error)
                                        .as_error_response(
                                            response.internal_response.version,
                                            response.request_id,
                                            response.connection_id,
                                            response.port,
                                        );
                                    message_bus.send(LayerTcpSteal::HttpResponse(response)).await;

                                    return Ok(());
                                }
                            }
                        }
                    }

                    None => vec![],
                };

                let response = response.map_body(|body| {
                    let mut complete = Vec::with_capacity(
                        body.head
                            .iter()
                            .filter_map(|frame| Some(frame.data_ref()?.len()))
                            .sum::<usize>()
                            + tail.len(),
                    );
                    for frame in body
                        .head
                        .into_iter()
                        .map(Frame::into_data)
                        .filter_map(Result::ok)
                    {
                        complete.extend(frame);
                    }
                    complete.extend(tail);
                    complete
                });

                message_bus
                    .send(LayerTcpSteal::HttpResponse(response))
                    .await;
            }

            Self::Framed(mut response) => {
                if let Some(mut incoming) = response.internal_response.body.tail.take() {
                    let start = Instant::now();
                    loop {
                        tokio::select! {
                            _ = message_bus.recv() => {
                                tracing::trace!("Message bus closed, exiting");
                                return Ok(());
                            },

                            result = incoming.next_frames() => match result {
                                Ok(data) => {
                                    response.internal_response.body.head.extend(data.frames);

                                    if data.is_last {
                                        tracing::trace!(
                                            elapsed_s = start.elapsed().as_secs_f32(),
                                            "Collected the whole response body."
                                        );
                                        break;
                                    }
                                },

                                Err(error) => {
                                    tracing::warn!(
                                        connection_id = response.connection_id,
                                        request_id = response.request_id,
                                        %error,
                                        "Failed to read the response body.",
                                    );

                                    let response = LocalHttpError::ReadBodyFailed(error)
                                        .as_error_response(
                                            response.internal_response.version,
                                            response.request_id,
                                            response.connection_id,
                                            response.port,
                                        );
                                    message_bus.send(LayerTcpSteal::HttpResponse(response)).await;

                                    return Ok(());
                                }
                            }
                        }
                    }
                };

                let response = response.map_body(|body| {
                    InternalHttpBody(
                        body.head
                            .into_iter()
                            .map(InternalHttpBodyFrame::from)
                            .collect(),
                    )
                });

                message_bus
                    .send(LayerTcpSteal::HttpResponseFramed(response))
                    .await;
            }

            Self::Chunked {
                connection_id,
                request_id,
                mut body,
            } => {
                let start = Instant::now();
                loop {
                    tokio::select! {
                        _ = message_bus.recv() => {
                            tracing::trace!("Message bus closed, exiting");
                            return Ok(())
                        },

                        result = body.next_frames() => match result {
                            Ok(data) => {
                                let message = LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(ChunkedHttpBody {
                                    frames: data.frames.into_iter().map(InternalHttpBodyFrame::from).collect(),
                                    is_last: data.is_last,
                                    connection_id,
                                    request_id,
                                }));
                                message_bus.send(message).await;

                                if data.is_last {
                                    tracing::trace!(
                                        elapsed_s = start.elapsed().as_secs_f32(),
                                        "Collected the whole response body."
                                    );
                                    break;
                                }
                            },

                            Err(error) => {
                                tracing::warn!(
                                    connection_id,
                                    request_id,
                                    %error,
                                    "Failed to read the response body.",
                                );

                                let message = LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Error(ChunkedHttpError {
                                    connection_id,
                                    request_id,
                                }));
                                message_bus.send(message).await;

                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
