//! `mirrord login`, `mirrord logout` and `mirrord whoami`.
//!
//! `mirrord login --url` logs in to an operator with the company identity provider ([`operator`])
//! or to mirrord Cloud, and stores the resulting login token in [`AuthStore`]. Which one the URL
//! is gets decided before any browser opens: an operator answers the login info route, mirrord
//! Cloud is the URL `MIRRORD_METALBEAR_CLOUD_URL` names.
//!
//! Both logins are a browser approval bound to a PKCE verifier ([RFC 7636]), following [RFC 8252]
//! for native apps:
//!
//! 1. The CLI generates a verifier, listens on an ephemeral loopback port, and opens the login
//!    page (the backend's `/auth-cli` page, or the operator's authorize route, which sends the
//!    browser on to the identity provider) with the verifier's S256 challenge and its callback URI.
//! 2. The user signs in if needed and approves. The backend or the operator signs a two-minute
//!    grant bound to the challenge and the callback, and the browser delivers it on loopback.
//! 3. The CLI calls the backend or the operator again to exchange the grant and the verifier for
//!    a login token.
//!
//! The verifier never leaves the CLI, so a grant intercepted on its way to the callback is useless
//! by itself.
//!
//! [RFC 7636]: https://www.rfc-editor.org/info/rfc7636/
//! [RFC 8252]: https://www.rfc-editor.org/info/rfc8252/

use std::{
    fmt,
    io::{self, IsTerminal},
    ops::Not,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Local, Utc};
use miette::Diagnostic;
use mirrord_operator::types::PaidFeatureGate;
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_sessions_manager_client::cloud_url_from_env;
use rand::RngExt;
use reqwest::{StatusCode, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::warn;
use url::{Host, Url};

use crate::{
    config::LoginArgs,
    data::{AuthStore, StoredLoginToken},
    login::callback::LoginCallbackServer,
};

mod active;
mod callback;
mod expiry;
mod logout;
mod operator;
mod whoami_command;

pub(crate) use active::apply_login_and_global_defaults;
pub(crate) use logout::logout_command;
pub(crate) use whoami_command::whoami_command;

/// 5 minutes – how long the CLI waits for the user to approve the login in the browser,
/// this including signing in.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Backend path where the user can log in.
const LOGIN_PAGE_PATH: &str = "auth-cli";
/// Backend path at which we can exchange the grant for the auth token.
const GRANT_EXCHANGE_PATH: &str = "api/v1/cli-login/token";
/// How long the CLI waits for the backend to exchange the grant.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
/// How much of the backend's response body is shown when it rejects the exchange.
const MAX_REJECTION_BODY_CHARS: usize = 512;

/// Implementation of the `mirrord login` command.
///
/// The URL decides the flow before any browser opens: a saved, unexpired operator login is
/// switched to on the spot; an operator that answers its login info route gets the SSO login; the
/// mirrord Cloud URL gets the Cloud login. Anything else is an error naming both options.
#[mirrord_macros::boxed_future]
pub(crate) async fn login_command(args: LoginArgs) -> Result<(), LoginError> {
    let mut progress = ProgressTracker::from_env("mirrord login");
    let url = backend_base_url(args.url)?;

    if let Some(login) = AuthStore::activate_operator_login(issuer(&url).to_owned())
        .await
        .map_err(LoginError::Store)?
    {
        progress.success(Some(&format!(
            "Logged in to {} as {} already, valid until {}. mirrord uses this operator now.",
            login.host(),
            login.account,
            local_time(login.expires_at),
        )));
        return Ok(());
    }

    if let Some(info) = operator::probe(&url).await? {
        return operator::login(&mut progress, url, info).await;
    }

    let cloud_url = cloud_url_from_env()
        .ok()
        .and_then(|cloud_url| Url::parse(&cloud_url).ok());
    if cloud_url.is_some_and(|cloud_url| issuer(&cloud_url) == issuer(&url)) {
        return cloud_login(&mut progress, url).await;
    }

    Err(LoginError::NotALoginUrl(url))
}

/// Login to mirrord Cloud at `backend_url`.
async fn cloud_login(progress: &mut ProgressTracker, backend_url: Url) -> Result<(), LoginError> {
    let login_page = backend_url.join(LOGIN_PAGE_PATH)?;
    let callback = LoginCallbackServer::prepare(&login_page).await?;
    let callback_uri = callback.url();
    let verifier = Verifier::random();

    let grant = approve_in_browser(progress, login_page, &verifier, callback).await?;

    // The grant is exchanged for the auth token.
    let mut subtask = progress.subtask("Exchanging grant for the auth token");
    let token = exchange_grant(&backend_url, &grant, &verifier, &callback_uri).await?;
    let claims = decode_claims(&token).ok_or_else(|| LoginError::BadToken("malformed".into()))?;
    // The token is filed under its issuer, so a backend must not be able to file a token under
    // another backend's entry.
    let expected_issuer = issuer(&backend_url);
    if claims.iss != expected_issuer {
        return Err(LoginError::BadToken(format!(
            "issued by {}, expected {expected_issuer}",
            claims.iss,
        )));
    }
    let expires_at = DateTime::from_timestamp(claims.exp, 0)
        .ok_or_else(|| LoginError::BadToken("expiration time out of valid range".into()))?;
    if expires_at <= Utc::now() {
        return Err(LoginError::BadToken(format!(
            "expired at {}",
            local_time(expires_at),
        )));
    }
    subtask.success(Some("Token received"));

    // The token is saved locally for reuse.
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
        local_time(expires_at),
    )));

    Ok(())
}

/// Opens `login_page` in the browser with the PKCE challenge and the callback's URI and state,
/// and waits for the browser to deliver the grant to `callback`.
async fn approve_in_browser(
    progress: &mut ProgressTracker,
    mut approval_url: Url,
    verifier: &Verifier,
    callback: LoginCallbackServer,
) -> Result<String, LoginError> {
    approval_url
        .query_pairs_mut()
        .append_pair("code_challenge", &verifier.code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("redirect_uri", &callback.url())
        .append_pair("state", callback.state_param());
    let approval_message = format!(
        "Approve the login in your browser. If it does not open, visit:\n\n{approval_url}\n"
    );
    // The progress is not drawn without a terminal, which is how agents run the CLI.
    if io::stderr().is_terminal().not() {
        eprintln!("{approval_message}");
    }
    let mut subtask = progress.subtask(&approval_message);
    if let Err(error) = opener::open_browser(approval_url.as_str()) {
        warn!(%error, "Failed to open the browser");
    }
    let grant = tokio::time::timeout(APPROVAL_TIMEOUT, callback.wait())
        .await
        .map_err(|_| LoginError::ApprovalTimeout)??;
    subtask.success(Some("Grant received"));
    Ok(grant)
}

/// The host (and port, when not the default) of a login URL, how messages name an operator.
fn host_of(url: &Url) -> String {
    issuer(url)
        .split_once("://")
        .map(|(_, host)| host.to_owned())
        .unwrap_or_else(|| url.to_string())
}

/// A point in time in the developer's time zone, how login messages show when a login ends.
fn local_time(time: DateTime<Utc>) -> impl fmt::Display {
    time.with_timezone(&Local).format("%Y-%m-%d %H:%M %Z")
}

/// Validates the backend URL passed in [`LoginArgs`], and turns it into a base for [`Url::join`].
///
/// The grant and the verifier are sent to the backend, so it must use TLS, unless it runs on this
/// machine. [`Url::join`] replaces the last path segment of a base that does not end with `/`, so
/// the slash is added to keep backends mounted under a path working.
pub(crate) fn backend_base_url(mut url: Url) -> Result<Url, LoginError> {
    let loopback = match url.host() {
        Some(Host::Domain(domain)) => domain == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(LoginError::InsecureBackendUrl(url));
    }

    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }

    Ok(url)
}

/// The issuer of the backend at `backend_url`, a base from [`backend_base_url`], which its login
/// tokens are filed under: the base URL without the trailing slash.
pub(crate) fn issuer(backend_url: &Url) -> &str {
    backend_url.as_str().trim_end_matches('/')
}

/// PKCE verifier (RFC 7636 §4.1) and its S256 challenge (RFC 7636 §4.2).
struct Verifier {
    /// base64url-encoded 32 random bytes.
    ///
    /// This never leaves the CLI before we get the grant.
    /// We pass it to the backend when exchanging the grant for the auth token,
    /// and the backend can check it against [`Self::code_challenge`]
    /// contained in grant claims.
    verifier: String,
    /// base64url-encoded SHA-256 of [`Self::verifier`].
    ///
    /// This is sent to the login page in URL params.
    code_challenge: String,
}

impl Verifier {
    /// Generates a fresh random verifier.
    fn random() -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 32]>());
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(&verifier));
        Self {
            verifier,
            code_challenge,
        }
    }
}

/// Exchanges `grant` for a login token.
///
/// The backend requires the verifier behind the grant's challenge, and the exact callback URI the
/// grant was issued for.
///
/// Redirects are not followed, so that the grant and the verifier are never sent anywhere but the
/// backend.
async fn exchange_grant(
    backend_url: &Url,
    grant: &str,
    verifier: &Verifier,
    callback_uri: &str,
) -> Result<String, LoginError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TokenRequest<'a> {
        grant: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    }

    let url = backend_url.join(GRANT_EXCHANGE_PATH)?;
    // Built fallibly: `reqwest::Client::new` panics when the system has no CA certificates.
    let response = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(EXCHANGE_TIMEOUT)
        .build()?
        .post(url)
        .json(&TokenRequest {
            grant,
            code_verifier: &verifier.verifier,
            redirect_uri: callback_uri,
        })
        .send()
        .await?;

    #[derive(Deserialize)]
    struct TokenResponse {
        token: String,
    }

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LoginError::Rejected {
            status,
            body: body.trim().chars().take(MAX_REJECTION_BODY_CHARS).collect(),
        });
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(From::from)
        .map(|response| response.token)
}

/// Claims of a login token.
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
/// check the issuer, file the token and show it to the user. Session managers verify the token
/// when it is used.
fn decode_claims(token: &str) -> Option<LoginTokenClaims> {
    let payload = token.split('.').nth(1)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// Errors that can occur during `mirrord login`.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum LoginError {
    /// Backend [`Url`] passed in CLI args was invalid.
    #[error("invalid backend URL: {0}")]
    InvalidBackendUrl(#[from] url::ParseError),
    /// Backend [`Url`] passed in CLI args does not use TLS, and is not on this machine.
    #[error("the backend URL must use HTTPS, unless it points to this machine: {0}")]
    InsecureBackendUrl(Url),
    /// Loopback callback handler failed.
    #[error("failed to listen on a loopback port: {0}")]
    LoopbackListen(#[source] io::Error),
    /// The browser delivered a refusal from the operator or the identity provider instead of a
    /// grant.
    #[error("the login was refused in the browser: {0}")]
    #[diagnostic(help(
        "Your identity provider or the operator did not complete the login. Fix what the message \
         names, then run `mirrord login` again."
    ))]
    Refused(String),
    /// Loopback callback handler was not called within the time limit.
    #[error("the login was not approved in the browser within {} minutes", APPROVAL_TIMEOUT.as_secs() / 60)]
    #[diagnostic(help("Run `mirrord login` again to start a new login."))]
    ApprovalTimeout,
    /// Failed to exchange the grant for a login token.
    #[error("failed to exchange the login approval for a login token: {0}")]
    Exchange(#[from] reqwest::Error),
    /// The backend refused to exchange the grant for a login token.
    #[error("the backend rejected the login with {status}: {body}")]
    #[diagnostic(help("Run `mirrord login` again to start a new login."))]
    Rejected { status: StatusCode, body: String },
    /// Exchanged grant for an invalid token.
    #[error("received a bad login token: {0}")]
    BadToken(String),
    /// Failed to store the token locally.
    #[error("failed to store the login token: {0}")]
    Store(#[source] io::Error),
    /// The URL is neither an operator with SSO on nor the mirrord Cloud URL.
    #[error("{0} is not a mirrord operator with SSO enabled, and it is not mirrord Cloud")]
    #[diagnostic(help(
        "Pass the operator URL your platform team set in the chart's `sso.url`, or the mirrord \
         Cloud URL. An operator with SSO off cannot be logged in to."
    ))]
    NotALoginUrl(Url),
    /// The operator could not be reached at all.
    #[error("could not reach {url}: {source}")]
    #[diagnostic(help("Check the URL and your network connection."))]
    Unreachable {
        url: Url,
        #[source]
        source: reqwest::Error,
    },
    /// The operator has SSO on but predates the CLI login routes.
    #[error("the operator at {host} does not support `mirrord login` yet")]
    #[diagnostic(help(
        "Ask your platform team to upgrade the operator to {} or newer.",
        operator::MIN_OPERATOR_VERSION
    ))]
    OperatorTooOld { host: String },
    /// The operator answered the login info route with something the CLI cannot read.
    #[error("the operator at {host} answered the login check with an unexpected {status} response")]
    #[diagnostic(help(
        "Run `mirrord login` again. If this keeps happening, check the operator's logs."
    ))]
    UnexpectedOperatorAnswer { host: String, status: StatusCode },
    /// The operator's license does not include SSO. Mapped to the paid-feature error every other
    /// gated feature produces.
    #[error("{0}")]
    NotLicensed(PaidFeatureGate),
    /// No login to that URL is saved, so there is nothing to log out of.
    #[error("you are not logged in to {0}")]
    #[diagnostic(help("Run `mirrord whoami` to see your logins."))]
    NotLoggedIn(String),
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn verifier_is_valid_pkce_verifier() {
        let verifier = Verifier::random();
        assert!((43..=128).contains(&verifier.verifier.len()));
        assert!(
            verifier
                .verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        );
        let challenge = URL_SAFE_NO_PAD.decode(&verifier.code_challenge).unwrap();
        assert_eq!(
            challenge.as_slice(),
            Sha256::digest(&verifier.verifier).as_slice(),
        );
    }

    #[rstest]
    #[case::root("https://app.metalbear.com", "https://app.metalbear.com/auth-cli")]
    #[case::path("https://host/backend", "https://host/backend/auth-cli")]
    #[case::path_with_slash("https://host/backend/", "https://host/backend/auth-cli")]
    #[case::localhost("http://localhost:3000", "http://localhost:3000/auth-cli")]
    #[case::loopback_ipv4("http://127.0.0.1:3000", "http://127.0.0.1:3000/auth-cli")]
    #[case::loopback_ipv6("http://[::1]:3000", "http://[::1]:3000/auth-cli")]
    fn backend_url_is_a_base_for_endpoints(#[case] url: &str, #[case] login_page: &str) {
        let backend_url = backend_base_url(url.parse().unwrap()).unwrap();
        assert_eq!(
            backend_url.join(LOGIN_PAGE_PATH).unwrap().as_str(),
            login_page
        );
    }

    #[rstest]
    #[case::remote_http("http://app.metalbear.com")]
    #[case::other_scheme("ftp://localhost")]
    fn insecure_backend_url_is_rejected(#[case] url: &str) {
        assert!(matches!(
            backend_base_url(url.parse().unwrap()),
            Err(LoginError::InsecureBackendUrl(..))
        ));
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
        assert_eq!(claims.email, "mieszko@polska.pl");
        assert_eq!(claims.organization_id, "org-polanie");
        assert_eq!(claims.exp, 86400);
        assert!(decode_claims("not-a-jwt").is_none());
    }
}
