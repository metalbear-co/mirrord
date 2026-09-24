//! `mirrord login`: authenticates the user with mirrord Cloud,
//! and stores the resulting login token in [`AuthStore`].
//!
//! The login is a browser approval bound to a [RFC 7636](https://www.rfc-editor.org/info/rfc7636/) PKCE verifier,
//! following [RFC 8252](https://www.rfc-editor.org/info/rfc8252/) for native apps:
//!
//! 1. The CLI generates a verifier, listens on an ephemeral loopback port, and opens the backend's
//!    `/auth-cli` page with the verifier's S256 challenge and its callback URI.
//! 2. The user signs in if needed and approves. The backend signs a two-minute grant bound to the
//!    challenge and the callback, and the browser delivers it on loopback.
//! 3. The CLI calls the backend again to exchange the grant and the verifier for a login token.
//!
//! The verifier never leaves the CLI, so a grant intercepted on its way to the callback is useless
//! by itself.

use std::{io, net::Ipv4Addr, time::Duration};

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Local};
use miette::Diagnostic;
use mirrord_progress::{Progress, ProgressTracker};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{net::TcpListener, sync::mpsc};
use tracing::warn;
use url::Url;

use crate::{
    config::LoginArgs,
    data::{AuthStore, StoredLoginToken},
};

/// 5 minutes – how long the CLI waits for the user to approve the login in the browser,
/// this including signing in.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Path on which the ephemeral loopback HTTP server spawned by the CLI accepts the grant.
const CALLBACK_PATH: &str = "/callback";

/// Implementation of the `mirrord login` command.
pub(crate) async fn login_command(args: LoginArgs) -> Result<(), LoginError> {
    let mut progress = ProgressTracker::from_env("mirrord login");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(LoginError::LoopbackListen)?;
    let callback_uri = format!(
        "http://{}{CALLBACK_PATH}",
        listener.local_addr().map_err(LoginError::LoopbackListen)?,
    );
    let verifier = generate_verifier();
    let state = URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 16]>());

    let (mut grant_rx, callback_task) = {
        let (grant_tx, grant_rx) = mpsc::channel(1);
        let tx_clone = grant_tx.clone();
        let router = Router::new()
            .route(CALLBACK_PATH, get(callback))
            .with_state(CallbackState {
                state: state.clone(),
                grant_tx,
            });
        let callback_task = tokio::spawn(
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { tx_clone.closed().await })
                .into_future(),
        );
        (grant_rx, callback_task)
    };

    let mut approval_url = args.url.join("auth-cli")?;
    approval_url
        .query_pairs_mut()
        .append_pair("code_challenge", &code_challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("redirect_uri", &callback_uri)
        .append_pair("state", &state);
    let mut subtask = progress.subtask(&format!(
        "Approve the login in your browser. If it does not open, visit:\n\n{approval_url}\n"
    ));
    if let Err(error) = opener::open_browser(approval_url.as_str()) {
        warn!(%error, "Failed to open the browser");
    }
    let grant = tokio::time::timeout(APPROVAL_TIMEOUT, grant_rx.recv())
        .await
        .ok()
        .flatten()
        .ok_or(LoginError::ApprovalTimeout)?;
    drop(grant_rx); // trigger graceful shutdown of the callback server
    subtask.success(Some("Grant received"));

    let mut subtask = progress.subtask("Exchanging grant for the auth token");
    let token = exchange_grant(&args.url, &grant, &verifier, &callback_uri).await?;
    let claims = decode_claims(&token).ok_or(LoginError::MalformedToken)?;
    let expires_at = DateTime::from_timestamp(claims.exp, 0).ok_or(LoginError::MalformedToken)?;
    subtask.success(Some("Token received"));

    let mut subtask = progress.subtask("Saving the token");
    AuthStore::save(
        claims.iss,
        claims.organization_id,
        StoredLoginToken {
            token: token.into(),
            email: claims.email.clone(),
            expires_at,
        },
    )
    .await
    .map_err(LoginError::Store)?;
    subtask.success(Some("Token saved"));

    progress.success(Some(&format!(
        "Logged in as {}. The login is valid until {}.",
        claims.email,
        expires_at.with_timezone(&Local).format("%Y-%m-%d %H:%M %Z"),
    )));

    let _ = callback_task.await;

    Ok(())
}

/// Generates a PKCE verifier (RFC 7636 §4.1).
///
/// base64url-encoded 32 random bytes.
fn generate_verifier() -> String {
    URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 32]>())
}

/// Returns the S256 challenge for the given `verifier` (RFC 7636 §4.2).
///
/// This is what:
/// 1. the CLI sends to the backend
/// 2. the backend includes in the grant
/// 3. the CLI cross-checks
fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier))
}

#[derive(Clone)]
struct CallbackState {
    /// Random value passed transparently through the approval page.
    ///
    /// Checked for equality in the callback handler.
    state: String,
    grant_tx: mpsc::Sender<String>,
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
        } if received_state == state.state => {
            // A full channel means a grant was already received.
            let _ = state.grant_tx.try_send(grant);
            (StatusCode::OK, Html(CALLBACK_RECEIVED_PAGE))
        }
        _ => (StatusCode::BAD_REQUEST, Html(CALLBACK_INVALID_PAGE)),
    }
}

/// Exchanges `grant` for a login token.
///
/// The backend requires the verifier behind the grant's challenge, and the exact callback URI the
/// grant was issued for.
async fn exchange_grant(
    backend_url: &Url,
    grant: &str,
    verifier: &str,
    callback_uri: &str,
) -> Result<String, LoginError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TokenRequest<'a> {
        grant: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    }

    let url = backend_url.join("api/v1/cli-login/token")?;
    let response = reqwest::Client::new()
        .post(url)
        .json(&TokenRequest {
            grant,
            code_verifier: verifier,
            redirect_uri: callback_uri,
        })
        .send()
        .await?;

    #[derive(Deserialize)]
    struct TokenResponse {
        token: String,
    }

    response
        .error_for_status()?
        .json::<TokenResponse>()
        .await
        .map_err(From::from)
        .map(|response| response.token)
}

/// Claims of a login token..
#[derive(Deserialize)]
struct LoginTokenClaims {
    iss: String,
    email: String,
    organization_id: String,
    exp: i64,
}

/// Reads the claims of a login token without verifying its signature.
///
/// The CLI receives the token directly from the backend over TLS, and only uses the claims to
/// file the token and show it to the user. Session managers verify the token when it is used.
fn decode_claims(token: &str) -> Option<LoginTokenClaims> {
    let payload = token.split('.').nth(1)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// Errors that can occur during `mirrord login`.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum LoginError {
    #[error("invalid backend URL: {0}")]
    InvalidBackendUrl(#[from] url::ParseError),

    #[error("failed to listen on a loopback port: {0}")]
    LoopbackListen(#[source] io::Error),

    #[error("the login was not approved in the browser within {} minutes", APPROVAL_TIMEOUT.as_secs() / 60)]
    #[diagnostic(help("Run `mirrord login` again to start a new login."))]
    ApprovalTimeout,

    #[error("failed to exchange the login approval for a login token: {0}")]
    Exchange(#[from] reqwest::Error),

    #[error("received a malformed login token")]
    MalformedToken,

    #[error("failed to store the login token: {0}")]
    Store(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_s256_of_the_verifier() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_valid_pkce_verifier() {
        let verifier = generate_verifier();

        assert!((43..=128).contains(&verifier.len()));
        assert!(
            verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        );
    }

    #[test]
    fn claims_are_decoded_from_the_payload() {
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "iss": "https://app.metalbear.com",
                "aud": "mirrord-cli",
                "sub": "mieszko",
                "email": "mieszko@polska.pl",
                "organization_id": "org-polanie",
                "iat": 0,
                "exp": 86400,
                "jti": "token-1",
            })
            .to_string(),
        );

        let claims = decode_claims(&format!("header.{payload}.signature")).unwrap();

        assert_eq!(claims.iss, "https://app.metalbear.com");
        assert_eq!(claims.email, "mieszko@example.com");
        assert_eq!(claims.organization_id, "org-polanie");
        assert_eq!(claims.exp, 86400);
        assert!(decode_claims("not-a-jwt").is_none());
    }
}
