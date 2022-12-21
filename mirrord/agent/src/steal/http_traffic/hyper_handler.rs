use core::{future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use http_body_util::BodyExt;
use hyper::{
    body::Incoming, client, header::HOST, http::HeaderValue, service::Service, Request, Response,
};
use mirrord_protocol::{ConnectionId, Port};
use reqwest::{Client, Url};
use tokio::{net::TcpStream, sync::mpsc::Sender};

use super::{error::HttpTrafficError, UnmatchedResponse};
use crate::{steal::StealerHttpRequest, util::ClientId};

#[derive(Debug)]
pub(super) struct HyperHandler {
    pub(super) filters: Arc<DashMap<ClientId, Regex>>,
    pub(super) captured_tx: Sender<StealerHttpRequest>,
    pub(super) unmatched_tx: Sender<UnmatchedResponse>,
    pub(crate) connection_id: ConnectionId,
    pub(crate) port: Port,
    pub(crate) original_destination: SocketAddr,
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

#[tracing::instrument(level = "debug", skip(tx))]
fn intercepted_request(
    mut request: Request<Incoming>,
    tx: Sender<UnmatchedResponse>,
    original_destination: SocketAddr,
) {
    tokio::spawn(async move {
        println!("original {:#?}", original_destination);
        let target_stream = TcpStream::connect(original_destination).await.unwrap();

        println!("Connecting to {:#?}", target_stream);
        let (mut request_sender, connection) =
            client::conn::http1::handshake(target_stream).await.unwrap();
        println!("hands shaked {:#?}", connection);

        tokio::spawn(async move {
            if let Err(fail) = connection.await {
                eprintln!("Error in connection: {}", fail);
            }
        });

        // TODO(alex) [mid] 2022-12-21: Does the host come in with the address of our intercepted
        // stream, or of the original destination already?
        //
        // Doing this only because the local tests are not handled by the stealer mechanism, so the
        // host comes with the wrong address.
        let proper_host = HeaderValue::from_str(&original_destination.to_string()).unwrap();
        request.headers_mut().insert(HOST, proper_host).unwrap();
        println!("request {:#?}", request);
        let intercepted_response = request_sender.send_request(request).await.unwrap();
        println!("Sent request {:#?}", intercepted_response);

        let response = UnmatchedResponse(intercepted_response);
        // TODO(alex) [high] 2022-12-20: Send this response back in the original stream.
        println!("Unmatched response {:#?}", response);
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
            intercepted_request(
                request,
                self.unmatched_tx.clone(),
                self.original_destination,
            );

            let response = async { Ok(Response::new("Unmatched!".to_string())) };
            Box::pin(response)
        }
    }
}
