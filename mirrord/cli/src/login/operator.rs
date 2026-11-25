//! Login to an operator through its public URL with the company identity provider.
//!
//! The operator is the OIDC client (it holds the client secret), so it hosts the redirect: the
//! browser goes to the operator's authorize route, the operator sends it to the identity provider,
//! takes it back on its callback route, and hands the CLI's loopback callback a grant bound to the
//! PKCE challenge. The CLI exchanges the grant and the verifier for a login token the operator
//! accepts as a bearer token for 24 hours. See [`mirrord_operator::sso`] for the routes.

use std::time::Duration;

use chrono::Utc;
use mirrord_operator::{
    sso::{
        AUTHORIZE_PATH, DONE_PATH, INFO_PATH, SSO_PATH, SsoLoginInfo, SsoTokenRequest,
        SsoTokenResponse, StoredOperatorLogin, TOKEN_PATH,
    },
    types::PaidFeatureGate,
};
use mirrord_progress::{Progress, ProgressTracker};
use reqwest::{StatusCode, redirect::Policy};
use url::Url;

use super::{
    EXCHANGE_TIMEOUT, LoginError, MAX_REJECTION_BODY_CHARS, Verifier, approve_in_browser, host_of,
    issuer, local_time,
};
use crate::{data::AuthStore, login::callback::LoginCallbackServer};

/// First operator version that serves the login routes. An operator with SSO on but without them
/// answers the login info route with the identity middleware's 401.
pub(super) const MIN_OPERATOR_VERSION: &str = "3.238.0";

/// Bound on the login info request, which runs before anything is shown to the developer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Asks the operator at `url` whether it accepts CLI logins, with no browser involved.
///
/// `None` when `url` is not an operator: the answer is not a Kubernetes `Status` the operator
/// would produce. An operator whose license lacks SSO answers the paid-feature gate, and one too
/// old for the login routes answers the identity middleware's 401; both are errors in their own
/// words, so the developer never gets sent to a browser that cannot log them in.
pub(super) async fn probe(url: &Url) -> Result<Option<SsoLoginInfo>, LoginError> {
    let host = host_of(url);
    let response = http_client(PROBE_TIMEOUT)?
        .get(sso_route(url, INFO_PATH)?)
        .send()
        .await
        .map_err(|source| LoginError::Unreachable {
            url: url.clone(),
            source,
        })?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|source| LoginError::Unreachable {
            url: url.clone(),
            source,
        })?;

    // An operator with SSO answers the route with its login info. Anything else answering 200
    // here (mirrord Cloud, a web server at a mistyped host) is not an operator.
    if status == StatusCode::OK {
        return Ok(serde_json::from_slice::<SsoLoginInfo>(&body).ok());
    }

    let Some(kube_status) = kube_status(&body) else {
        return Ok(None);
    };
    if let Some(gate) = PaidFeatureGate::from_status(&kube_status) {
        return Err(LoginError::NotLicensed(gate));
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(LoginError::OperatorTooOld { host });
    }
    Err(LoginError::UnexpectedOperatorAnswer { host, status })
}

/// A Kubernetes `Status` body, which every error answer of the operator is. `Status` has defaults
/// for every field, so an empty object would parse too; the code and reason the operator always
/// sets tell a real one apart.
fn kube_status(body: &[u8]) -> Option<kube::core::Status> {
    serde_json::from_slice::<kube::core::Status>(body)
        .ok()
        .filter(|status| status.code != 0 && !status.reason.is_empty())
}

/// Logs in to the operator at `url` through the browser and saves the login as the active one.
pub(super) async fn login(
    progress: &mut ProgressTracker,
    url: Url,
    info: SsoLoginInfo,
) -> Result<(), LoginError> {
    let host = host_of(&url);
    progress.info(&format!(
        "Signing in to {host} (operator {}) with {}",
        info.operator_version, info.issuer_url
    ));

    let callback = LoginCallbackServer::prepare(&sso_route(&url, DONE_PATH)?).await?;
    let callback_uri = callback.url();
    let verifier = Verifier::random();
    let grant = approve_in_browser(
        progress,
        sso_route(&url, AUTHORIZE_PATH)?,
        &verifier,
        callback,
    )
    .await?;

    let mut subtask = progress.subtask("Exchanging grant for the login token");
    let response = exchange_grant(&url, &grant, &verifier, &callback_uri).await?;
    if response.expires_at <= Utc::now() {
        return Err(LoginError::BadToken(format!(
            "expired at {}",
            local_time(response.expires_at)
        )));
    }
    subtask.success(Some("Login token received"));

    let mut subtask = progress.subtask("Saving the login");
    AuthStore::save_operator_login(
        issuer(&url).to_owned(),
        StoredOperatorLogin {
            token: response.token,
            username: response.username,
            account: response.account.clone(),
            expires_at: response.expires_at,
            logged_in_at: Utc::now(),
        },
    )
    .await
    .map_err(LoginError::Store)?;
    subtask.success(Some("Login saved"));

    progress.success(Some(&format!(
        "Logged in to {host} as {}. The login is valid until {}. mirrord uses this operator now.",
        response.account,
        local_time(response.expires_at),
    )));
    Ok(())
}

/// Exchanges `grant` for a login token at the operator's token route.
///
/// The operator requires the verifier behind the grant's challenge and the exact callback URI the
/// grant was issued for. Redirects are not followed, so the grant and the verifier never go
/// anywhere but the operator.
async fn exchange_grant(
    url: &Url,
    grant: &str,
    verifier: &Verifier,
    callback_uri: &str,
) -> Result<SsoTokenResponse, LoginError> {
    let response = http_client(EXCHANGE_TIMEOUT)?
        .post(sso_route(url, TOKEN_PATH)?)
        .json(&SsoTokenRequest {
            grant: grant.to_owned(),
            code_verifier: verifier.verifier.clone(),
            redirect_uri: callback_uri.to_owned(),
        })
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LoginError::Rejected {
            status,
            body: body.trim().chars().take(MAX_REJECTION_BODY_CHARS).collect(),
        });
    }

    response
        .json::<SsoTokenResponse>()
        .await
        .map_err(From::from)
}

/// Built fallibly: `reqwest::Client::new` panics when the system has no CA certificates.
fn http_client(timeout: Duration) -> Result<reqwest::Client, LoginError> {
    Ok(reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(timeout)
        .build()?)
}

/// One of the operator's login routes under `url`, a base from
/// [`super::backend_base_url`].
fn sso_route(url: &Url, path: &str) -> Result<Url, LoginError> {
    Ok(url.join(&format!("{SSO_PATH}/{path}"))?)
}

/// How messages name the operator: its host, with the port when it is not the default.
#[cfg(test)]
mod tests {
    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use kube::core::response::StatusSummary;
    use mirrord_operator::{sso::OperatorLoginToken, types::RequiredPlan};
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;
    use crate::login::backend_base_url;

    /// Serves `router` on a loopback port and returns the base URL the CLI would be given.
    async fn serve(router: Router) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router).into_future());
        backend_base_url(format!("http://{addr}").parse().unwrap()).unwrap()
    }

    fn status_body(
        code: StatusCode,
        reason: &str,
        cause: Option<serde_json::Value>,
    ) -> impl IntoResponse {
        let details = cause.map(|cause| json!({ "causes": [cause] }));
        let status = json!({
            "status": StatusSummary::Failure,
            "message": "refused",
            "reason": reason,
            "code": code.as_u16(),
            "details": details,
        });
        (code, Json(status))
    }

    #[tokio::test]
    async fn probe_recognizes_an_operator_that_accepts_logins() {
        let url = serve(Router::new().route(
            &format!("/{SSO_PATH}/{INFO_PATH}"),
            get(|| async {
                Json(SsoLoginInfo {
                    operator_version: "3.238.0".to_owned(),
                    issuer_url: "https://login.example.com".to_owned(),
                })
            }),
        ))
        .await;

        let info = probe(&url).await.unwrap().unwrap();
        assert_eq!(info.operator_version, "3.238.0");
        assert_eq!(info.issuer_url, "https://login.example.com");
    }

    /// A host that is not an operator (mirrord Cloud, a typo) answers nothing the operator would.
    #[tokio::test]
    async fn probe_treats_a_non_operator_answer_as_not_an_operator() {
        let url = serve(Router::new().fallback(|| async { (StatusCode::NOT_FOUND, "nope") })).await;
        assert!(probe(&url).await.unwrap().is_none());

        let url = serve(Router::new().fallback(|| async { Json(json!({"hello": "world"})) })).await;
        assert!(probe(&url).await.unwrap().is_none());
    }

    /// The operator's paid-feature gate becomes the paid-feature error, before any browser.
    #[tokio::test]
    async fn probe_surfaces_the_paid_feature_gate() {
        let gate = PaidFeatureGate {
            feature_config_key: "sso".to_owned(),
            required_plan: RequiredPlan::Enterprise,
        };
        let cause = serde_json::to_value(gate.to_status_cause().unwrap()).unwrap();
        let url = serve(Router::new().fallback(move || async move {
            status_body(
                StatusCode::FORBIDDEN,
                PaidFeatureGate::STATUS_REASON,
                Some(cause),
            )
        }))
        .await;

        let error = probe(&url).await.unwrap_err();
        let LoginError::NotLicensed(gate) = error else {
            panic!("{error:?}");
        };
        assert_eq!(gate.feature_config_key, "sso");
        assert_eq!(gate.required_plan, RequiredPlan::Enterprise);
    }

    /// An operator with SSO on but no login routes answers with its identity middleware's 401.
    #[tokio::test]
    async fn probe_names_an_operator_too_old_for_the_login() {
        let url =
            serve(Router::new().fallback(|| async {
                status_body(StatusCode::UNAUTHORIZED, "Unauthorized", None)
            }))
            .await;

        let error = probe(&url).await.unwrap_err();
        assert!(
            matches!(error, LoginError::OperatorTooOld { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn probe_reports_an_unreachable_operator() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = backend_base_url(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        )
        .unwrap();
        drop(listener);

        let error = probe(&url).await.unwrap_err();
        assert!(matches!(error, LoginError::Unreachable { .. }), "{error:?}");
    }

    /// The exchange sends the grant, the verifier and the callback the grant was issued for, and
    /// reads the token the operator answers with.
    #[tokio::test]
    async fn exchange_sends_the_verifier_and_reads_the_token() {
        let verifier = Verifier::random();
        let expected_verifier = verifier.verifier.clone();
        let url =
            serve(
                Router::new()
                    .route(
                        &format!("/{SSO_PATH}/{TOKEN_PATH}"),
                        post(
                            |State(expected): State<String>,
                             Json(request): Json<SsoTokenRequest>| async move {
                                if request.grant != "the-grant"
                                    || request.code_verifier != expected
                                    || request.redirect_uri != "http://127.0.0.1:1/callback"
                                {
                                    return (StatusCode::BAD_REQUEST, "wrong request")
                                        .into_response();
                                }
                                Json(SsoTokenResponse {
                                    token: OperatorLoginToken::from("login-token".to_owned()),
                                    username: "dev@corp.example.com".to_owned(),
                                    account: "dev@corp.example.com".to_owned(),
                                    expires_at: Utc::now(),
                                })
                                .into_response()
                            },
                        ),
                    )
                    .with_state(expected_verifier),
            )
            .await;

        let response = exchange_grant(&url, "the-grant", &verifier, "http://127.0.0.1:1/callback")
            .await
            .unwrap();
        assert_eq!(response.token.expose_secret(), "login-token");
        assert_eq!(response.account, "dev@corp.example.com");

        let error = exchange_grant(
            &url,
            "other-grant",
            &verifier,
            "http://127.0.0.1:1/callback",
        )
        .await
        .unwrap_err();
        let LoginError::Rejected { status, body } = error else {
            panic!("{error:?}");
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "wrong request");
    }

    #[test]
    fn routes_live_under_the_operator_url() {
        let url = backend_base_url("https://mirrord.corp.example.com".parse().unwrap()).unwrap();
        assert_eq!(
            sso_route(&url, AUTHORIZE_PATH).unwrap().as_str(),
            "https://mirrord.corp.example.com/apis/operator.metalbear.co/sso/authorize"
        );
        assert_eq!(host_of(&url), "mirrord.corp.example.com");
        let url = backend_base_url("https://localhost:8443/".parse().unwrap()).unwrap();
        assert_eq!(host_of(&url), "localhost:8443");
    }
}
