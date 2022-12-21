use core::{future::Future, pin::Pin};
use std::sync::Arc;

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use hyper::{body::Incoming, service::Service, Request, Response};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::sync::mpsc::Sender;

use super::{error::HttpTrafficError, PassthroughRequest};
use crate::{steal::StealerHttpRequest, util::ClientId};

#[derive(Debug)]
pub(super) struct HyperHandler {
    pub(super) filters: Arc<DashMap<ClientId, Regex>>,
    pub(super) captured_tx: Sender<StealerHttpRequest>,
    pub(super) passthrough_tx: Sender<PassthroughRequest>,
    pub(crate) connection_id: ConnectionId,
    pub(crate) port: Port,
    pub(crate) request_id: RequestId,
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

impl Service<Request<Incoming>> for HyperHandler {
    type Response = Response<String>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    // TODO(alex) [mid] 2022-12-13: Do we care at all about what is sent from here as a response to
    // our client duplex stream?
    #[tracing::instrument(level = "debug", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
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
                    request_id: self.request_id,
                    request,
                },
                self.captured_tx.clone(),
            );

            let response = async { Ok(Response::new("Captured!".to_string())) };
            self.request_id += 1;
            Box::pin(response)
        } else {
            spawn_send(PassthroughRequest(request), self.passthrough_tx.clone());
            self.request_id += 1;

            let response = async { Ok(Response::new("Passthrough!".to_string())) };
            Box::pin(response)
        }
    }
}
