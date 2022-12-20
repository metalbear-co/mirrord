use core::{future::Future, pin::Pin};
use std::sync::Arc;

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use http_body_util::BodyExt;
use hyper::{
    body::{Body, Buf, Incoming},
    service::Service,
    Request, Response,
};
use mirrord_protocol::{ConnectionId, Port};
use reqwest::{Client, Url};
use tokio::sync::mpsc::Sender;

use super::{error::HttpTrafficError, PassthroughResponse};
use crate::{steal::StealerHttpRequest, util::ClientId};

#[derive(Debug)]
pub(super) struct HyperHandler {
    pub(super) filters: Arc<DashMap<ClientId, Regex>>,
    pub(super) captured_tx: Sender<StealerHttpRequest>,
    pub(super) passthrough_tx: Sender<PassthroughResponse>,
    pub(crate) connection_id: ConnectionId,
    pub(crate) port: Port,
}

// TODO(alex) [low] 2022-12-13: Come back to these docs to create a link to where this is in the
// agent.
//
/// Creates a task to send a message (either [`StealerHttpRequest`] or [`PassthroughRequest`]) to
/// the receiving end that lives in the stealer.
///
/// As the [`hyper::service::Service`] trait doesn't support `async fn` for the [`Service::call`]
/// method, we use this helper function that allows us to send a `value: T` via a `Sender<T>`
/// without the need to call `await`.
#[tracing::instrument(level = "debug", skip(tx))]
fn spawn_send<T>(value: T, tx: Sender<T>)
where
    T: Send + 'static + core::fmt::Debug,
    HttpTrafficError: From<tokio::sync::mpsc::error::SendError<T>>,
{
    tokio::spawn(async move { tx.send(value).map_err(HttpTrafficError::from).await });
}

// #[tracing::instrument(level = "debug", skip(tx))]
fn intercepted_request(request: Request<Incoming>, tx: Sender<PassthroughResponse>) {
    let (headers, body) = request.into_parts();
    let client = Client::new();

    tokio::spawn(async move {
        let body_bytes = body.collect().await.unwrap().to_bytes();

        let uri = headers.uri.to_string();
        let url = Url::parse(&uri).unwrap();
        let intercepted_request = client
            .request(headers.method, url)
            .headers(headers.headers)
            .body(body_bytes)
            .build()
            .unwrap();

        let intercepted_response = client
            .execute(intercepted_request)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();

        let response = PassthroughResponse(intercepted_response);
        // TODO(alex) [high] 2022-12-20: Send this response back in the original stream.
        tx.send(response).map_err(HttpTrafficError::from).await
    });
}

impl Service<Request<Incoming>> for HyperHandler {
    type Response = Response<String>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    // TODO(alex) [mid] 2022-12-13: Do we care at all about what is sent from here as a response to
    // our client duplex stream?
    // #[tracing::instrument(level = "debug", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        // TODO(alex) [mid] 2022-12-20: The `Incoming` to `Bytes` conversion should be done here
        // for both cases, as that's what we care about.
        if let Some(client_id) = request
            .headers()
            .iter()
            .map(|(header_name, header_value)| {
                format!("{}={}", header_name, header_value.to_str().unwrap())
            })
            .find_map(|header| {
                self.filters.iter().find_map(|filter| {
                    if filter.is_match(&header).unwrap() {
                        Some(filter.key().clone())
                    } else {
                        None
                    }
                })
            })
        {
            spawn_send(
                StealerHttpRequest {
                    port: self.port,
                    connection_id: self.connection_id,
                    client_id,
                    request,
                },
                self.captured_tx.clone(),
            );

            let response = async { Ok(Response::new("Captured!".to_string())) };
            Box::pin(response)
        } else {
            intercepted_request(request, self.passthrough_tx.clone());

            let response = async { Ok(Response::new("Passthrough!".to_string())) };
            Box::pin(response)
        }
    }
}
