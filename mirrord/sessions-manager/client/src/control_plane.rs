//! HTTP/SSE control-plane transport.

mod api;
mod event;

use std::{pin::Pin, sync::Arc, time::Duration};

pub(crate) use api::AssignmentSubscription;
use api::{ControlPlaneApi, ControlPlaneEndpoint};
pub(crate) use event::ControlPlaneEvent;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    config::SessionsManagerConfig, credentials::CredentialProvider,
    error::SessionsManagerClientError, retry::run_interruptible,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) type ControlPlaneEventStream =
    Pin<Box<dyn Stream<Item = Result<ControlPlaneEvent, SessionsManagerClientError>> + Send>>;

#[derive(Clone)]
pub(crate) struct HttpControlPlaneClient {
    client: reqwest::Client,
    api: ControlPlaneApi,
    config: SessionsManagerConfig,
    credentials: Arc<dyn CredentialProvider>,
}

impl HttpControlPlaneClient {
    pub(crate) fn new(
        config: &SessionsManagerConfig,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<Self, SessionsManagerClientError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            api: ControlPlaneApi::new(config.base_url.clone()),
            config: config.clone(),
            credentials,
        })
    }

    pub(crate) async fn subscribe_assignments(
        &self,
        subscription: &AssignmentSubscription,
        cancellation: &CancellationToken,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        let endpoint = self.api.endpoint(ControlPlaneEndpoint::Assignments {
            environment: &self.config.environment,
            service: &self.config.service,
        })?;
        let request = self
            .client
            .get(endpoint)
            .query(subscription)
            .headers(self.credentials.headers()?)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send();
        let deadline = Instant::now() + RESPONSE_HEADER_TIMEOUT;
        let response = run_interruptible(cancellation, Some(deadline), request).await??;
        if !response.status().is_success() {
            return Err(SessionsManagerClientError::HttpStatus(response.status()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        if !content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        }) {
            return Err(SessionsManagerClientError::InvalidContentType(
                content_type.map(str::to_owned),
            ));
        }

        let api = self.api.clone();
        Ok(Box::pin(response.bytes_stream().eventsource().filter_map(
            move |event| {
                let api = api.clone();
                async move {
                    match event {
                        Ok(event) => api.decode_event(event).transpose(),
                        Err(error) => Some(Err(SessionsManagerClientError::Sse(error.to_string()))),
                    }
                }
            },
        )))
    }
}
