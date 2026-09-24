//! Implementation of the loopback callback HTTP server used in HTTP login.
//!
//! The server is hit after the user logs in and the browser redirects to
//! [`LoginCallbackServer::url`], providing the grant in query params.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    Router,
    extract::{Query, State},
    response::Html,
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{FutureExt, future::BoxFuture};
use http::StatusCode;
use serde::Deserialize;
use tokio::{net::TcpListener, sync::SetOnce};

use crate::login::LoginError;

/// Loopback callback server for `mirrord login`, used to deliver the grant back to the CLI.
///
/// Exposes one GET endpoint at [`Self::CALLBACK_PATH`],
/// expecting `grant` and `state` query params.
pub struct LoginCallbackServer {
    state: CallbackState,
    addr: SocketAddr,
    server: BoxFuture<'static, io::Result<()>>,
}

impl LoginCallbackServer {
    pub const CALLBACK_PATH: &str = "/callback";

    pub async fn prepare() -> Result<Self, LoginError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(LoginError::LoopbackListen)?;
        let addr = listener.local_addr().map_err(LoginError::LoopbackListen)?;
        let grant = Arc::new(SetOnce::default());
        let state = CallbackState {
            state_param: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
            grant: grant.clone(),
        };
        let router = Router::new()
            .route(Self::CALLBACK_PATH, get(callback))
            .with_state(state.clone());
        let server = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                // Once the grant is delivered, we can gracefully shutdown the server.
                let _ = grant.wait().await;
            })
            .into_future()
            .boxed();

        Ok(Self {
            state,
            addr,
            server,
        })
    }

    /// Returns the full URL of the callback endpoint.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.addr, Self::CALLBACK_PATH)
    }

    /// Returns the expected value of the `state` query param.
    pub fn state_param(&self) -> &str {
        &self.state.state_param
    }

    /// Waits until the callback is called, and returns the grant.
    ///
    /// This method drives IO of the server.
    pub async fn wait(mut self) -> Result<String, LoginError> {
        let grant = tokio::select! {
            biased;
            grant = self.state.grant.wait() => grant,
            result = &mut self.server => {
                let error = result
                    .err()
                    .unwrap_or_else(|| io::Error::other("server task unexpectedly finished"));
                return Err(LoginError::LoopbackListen(error));
            },
        };
        // Grant delivery triggered graceful shutdown of the HTTP server.
        // We wait for it to finish, ensuring that the browser can receive the response.
        let _ = self.server.await;
        Ok(grant.clone())
    }
}

#[derive(Clone)]
struct CallbackState {
    state_param: String,
    grant: Arc<SetOnce<String>>,
}

#[derive(Deserialize)]
struct CallbackParams {
    grant: Option<String>,
    state: Option<String>,
}

const CALLBACK_RECEIVED_PAGE: &str = "<!DOCTYPE html><html><head><title>mirrord login</title>\
    </head><body><p>mirrord received the login approval. You can close this tab and return to \
    your terminal.</p></body></html>";

const CALLBACK_INVALID_PAGE: &str = "<!DOCTYPE html><html><head><title>mirrord login</title>\
    </head><body><p>This is not a valid mirrord login approval. Run <code>mirrord login</code> \
    to start a new login.</p></body></html>";

/// Receives the grant from the browser, which the approval page redirects to the callback URI.
async fn callback(
    State(state): State<CallbackState>,
    Query(params): Query<CallbackParams>,
) -> (StatusCode, Html<&'static str>) {
    match params {
        CallbackParams {
            grant: Some(grant),
            state: Some(received_state),
        } if received_state == state.state_param => {
            let _ = state.grant.set(grant);
            (StatusCode::OK, Html(CALLBACK_RECEIVED_PAGE))
        }
        _ => (StatusCode::BAD_REQUEST, Html(CALLBACK_INVALID_PAGE)),
    }
}

#[cfg(test)]
mod tests {
    use crate::login::callback::LoginCallbackServer;

    #[tokio::test]
    async fn accepts_only_valid_state_param() {
        let callback = LoginCallbackServer::prepare().await.unwrap();
        let url = callback.url();
        let state_param = callback.state_param().to_owned();

        let callback = tokio::spawn(async move { callback.wait().await.unwrap() });

        let response = reqwest::Client::new()
            .get(&url)
            .query(&[
                ("grant", "invalid"),
                ("state", &format!("{state_param}nope")),
            ])
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());

        reqwest::Client::new()
            .get(url)
            .query(&[("grant", "valid"), ("state", &state_param)])
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert_eq!(callback.await.unwrap(), "valid");
    }
}
