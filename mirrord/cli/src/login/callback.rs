//! Implementation of the loopback callback HTTP server used in HTTP login.
//!
//! The server is hit after the user logs in and the browser redirects to
//! [`LoginCallbackServer::url`], providing the grant in query params.
//!
//! The server does not render any pages. It redirects the browser back to the backend's login
//! page, with a `status` query param telling the page what to show. This keeps what the user
//! sees consistent with the rest of the web app.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    Router,
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Redirect},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{FutureExt, future::BoxFuture};
use serde::Deserialize;
use tokio::{net::TcpListener, sync::SetOnce};
use url::Url;

use crate::login::LoginError;

/// Loopback callback server for `mirrord login`, used to deliver the grant back to the CLI.
///
/// Exposes one GET endpoint at [`Self::CALLBACK_PATH`],
/// expecting `grant` and `state` query params.
/// Every request is redirected to the login page, see the module docs.
pub struct LoginCallbackServer {
    state: CallbackState,
    addr: SocketAddr,
    server: BoxFuture<'static, io::Result<()>>,
}

impl LoginCallbackServer {
    pub const CALLBACK_PATH: &str = "/callback";

    /// Binds the server to an ephemeral loopback port.
    ///
    /// `login_page` is the backend's login page, where the browser is redirected after the
    /// callback.
    pub async fn prepare(login_page: &Url) -> Result<Self, LoginError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(LoginError::LoopbackListen)?;
        let addr = listener.local_addr().map_err(LoginError::LoopbackListen)?;
        let grant = Arc::new(SetOnce::default());
        let state = CallbackState {
            state_param: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
            grant: grant.clone(),
            received_redirect: status_url(login_page, STATUS_RECEIVED),
            invalid_redirect: status_url(login_page, STATUS_INVALID),
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
    /// Where the browser is sent after delivering the grant.
    received_redirect: String,
    /// Where the browser is sent after a request without a valid grant or `state`.
    invalid_redirect: String,
}

#[derive(Deserialize)]
struct CallbackParams {
    grant: Option<String>,
    state: Option<String>,
}

/// `status` query param value telling the login page that the CLI received the grant.
const STATUS_RECEIVED: &str = "received";

/// `status` query param value telling the login page that the CLI rejected the callback.
const STATUS_INVALID: &str = "invalid";

/// Returns `login_page` with the given `status` query param.
fn status_url(login_page: &Url, status: &str) -> String {
    let mut url = login_page.clone();
    url.query_pairs_mut().append_pair("status", status);
    url.into()
}

/// Receives the grant from the browser, which the approval page redirects to the callback URI.
async fn callback(
    State(state): State<CallbackState>,
    Query(params): Query<CallbackParams>,
) -> impl IntoResponse {
    let redirect = match params {
        CallbackParams {
            grant: Some(grant),
            state: Some(received_state),
        } if received_state == state.state_param => {
            let _ = state.grant.set(grant);
            &state.received_redirect
        }
        _ => &state.invalid_redirect,
    };

    // The callback URL carries the grant, so it must not leak to the login page as the referrer.
    (
        [(header::REFERRER_POLICY, "no-referrer")],
        Redirect::to(redirect),
    )
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use reqwest::{Response, redirect::Policy};

    use super::*;

    const LOGIN_PAGE: &str = "https://app.metalbear.com/auth-cli";

    fn assert_redirected(response: &Response, status: &str) {
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[header::LOCATION],
            format!("{LOGIN_PAGE}?status={status}").as_str()
        );
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    }

    #[tokio::test]
    async fn accepts_only_valid_state_param() {
        let callback = LoginCallbackServer::prepare(&LOGIN_PAGE.parse().unwrap())
            .await
            .unwrap();
        let url = callback.url();
        let state_param = callback.state_param().to_owned();

        let callback = tokio::spawn(async move { callback.wait().await.unwrap() });
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap();

        let response = client
            .get(&url)
            .query(&[
                ("grant", "invalid"),
                ("state", &format!("{state_param}nope")),
            ])
            .send()
            .await
            .unwrap();
        assert_redirected(&response, STATUS_INVALID);

        let response = client
            .get(url)
            .query(&[("grant", "valid"), ("state", &state_param)])
            .send()
            .await
            .unwrap();
        assert_redirected(&response, STATUS_RECEIVED);
        assert_eq!(callback.await.unwrap(), "valid");
    }
}
