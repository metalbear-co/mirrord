//! What a standalone intproxy presents to sessions-manager, with the agent's API key configured in
//! the same environment.
//!
//! The environment is process-wide, so this lives in its own test binary instead of next to the
//! unit tests, which expect the API key to be unset.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    http::{StatusCode, Uri, header::AUTHORIZATION},
    response::IntoResponse,
};
use futures::future::{BoxFuture, FutureExt};
use mirrord_sessions_manager_client::{
    DirectTransport, IntproxyClient, LoginToken, LoginTokenSource, ServiceScope,
    SessionsManagerClientError, SessionsManagerConnectInfo, SessionsManagerEndpoint,
};
use tokio::net::TcpListener;

struct FixedLogin;

impl LoginTokenSource for FixedLogin {
    fn login_token(&self) -> BoxFuture<'_, Result<Option<LoginToken>, SessionsManagerClientError>> {
        std::future::ready(Ok(Some(LoginToken::from("login-token".to_owned())))).boxed()
    }
}

/// The intproxy authenticates with the login token and never exchanges the API key, which
/// sessions-manager refuses from an intproxy, and a refused login points the user at
/// `mirrord login` rather than at a bare status.
#[tokio::test]
async fn intproxy_presents_the_login_token_and_never_the_api_key() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let app = Router::new().fallback(move |uri: Uri, headers: axum::http::HeaderMap| {
        let recorded = recorded.clone();
        async move {
            let authorization = headers
                .get(AUTHORIZATION)
                .map(|value| value.to_str().unwrap().to_owned());
            recorded
                .lock()
                .unwrap()
                .push((uri.path().to_owned(), authorization));
            StatusCode::UNAUTHORIZED.into_response()
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // SAFETY: this binary's only test sets these before anything reads the environment.
    unsafe {
        std::env::set_var("MIRRORD_METALBEAR_CLOUD_URL", format!("http://{address}"));
        std::env::set_var("MIRRORD_SESSIONS_MANAGER_API_KEY", "metalbear_key_test");
    }

    let endpoint = SessionsManagerEndpoint::resolve(Some(&format!("http://{address}/sm"))).unwrap();
    let transport =
        DirectTransport::intproxy_from_env(endpoint, Some(Arc::new(FixedLogin))).unwrap();
    let client = IntproxyClient::with_transport(
        SessionsManagerConnectInfo {
            scope: ServiceScope {
                environment: "staging".to_owned(),
                service: "api".to_owned(),
            },
            user_session_id: "session".to_owned(),
            replica_filter: None,
        },
        transport,
    )
    .unwrap();

    let error = client.connect(Duration::from_secs(5)).await.unwrap_err();

    assert!(
        matches!(error, SessionsManagerClientError::LoginTokenRejected),
        "a 401 should be reported as a refused login, got {error:?}"
    );
    let requests = requests.lock().unwrap();
    assert!(!requests.is_empty());
    for (path, authorization) in requests.iter() {
        assert!(path.starts_with("/sm/"), "unexpected request to {path}");
        assert_eq!(authorization.as_deref(), Some("Bearer login-token"));
    }
}
