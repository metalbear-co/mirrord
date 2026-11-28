//! `mirrord up` under an operator login (`mirrord login --url`), on a machine with no
//! kubeconfig: it looks the services' workloads up through the operator, never through a
//! kubeconfig.

use std::{
    ops::Not,
    path::Path,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{TimeDelta, Utc};
use mirrord_command::resolve_tokio_command;
use mirrord_operator::sso::{OperatorLoginStore, OperatorLoginToken, StoredOperatorLogin};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};

/// One service with no `target.path`, so `mirrord up` has to look its workload up.
const MISSING_TARGET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/testdata/up-missing-target.yaml"
);

const TOKEN: &str = "e2e-login-token";

/// The path the operator serves its target listing on for namespace `default`, the namespace a
/// login's client defaults to.
const TARGETS_PATH: &str = "/apis/operator.metalbear.co/v1/namespaces/default/targets";

/// Each request a fake operator got: its path and `Authorization` header.
type Requests = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// How a fake operator answers the target listing.
#[derive(Clone, Copy)]
enum Targets {
    /// No targets in the namespace.
    None,
    /// RBAC refuses the user `list` on targets there.
    Forbidden,
}

/// A fake operator on a loopback port that answers the target listing as `targets` says, and
/// every other request with an empty list, and records every request.
async fn fake_operator(targets: Targets) -> (String, Requests, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Requests::default();
    let recorded = requests.clone();
    let router = Router::new().fallback(move |request: Request| {
        let recorded = recorded.clone();
        async move {
            let authorization = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let path = request.uri().path().to_owned();
            recorded.lock().unwrap().push((path.clone(), authorization));
            match targets {
                Targets::Forbidden if path == TARGETS_PATH => forbidden(),
                _ => Json(json!({"apiVersion": "v1", "kind": "List", "metadata": {}, "items": []}))
                    .into_response(),
            }
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, requests, server)
}

/// The `403` kube-apiserver answers a forbidden `list`, which the operator passes on.
fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "targets.operator.metalbear.co is forbidden",
            "reason": "Forbidden",
            "code": 403
        })),
    )
        .into_response()
}

/// The words of `text`, with the line breaks and the border the error report wraps long lines
/// behind taken out, so a message can be found whatever the terminal width.
fn unwrapped(text: &str) -> String {
    text.split_whitespace()
        .filter(|word| *word != "\u{2502}")
        .collect::<Vec<_>>()
        .join(" ")
}

/// Saves a login to the operator at `url` in `home`'s `~/.mirrord/auth.json`, the way
/// `mirrord login --url` does, expiring at `expires_in` from now.
fn save_login(home: &Path, url: &str, expires_in: TimeDelta) {
    let store = OperatorLoginStore {
        operators: [(
            url.to_owned(),
            StoredOperatorLogin {
                token: OperatorLoginToken::from(TOKEN.to_owned()),
                username: "dev@corp.example.com".to_owned(),
                account: "dev@corp.example.com".to_owned(),
                expires_at: Utc::now() + expires_in,
                logged_in_at: Utc::now(),
            },
        )]
        .into(),
        active_operator: Some(url.to_owned()),
    };
    let mirrord_dir = home.join(".mirrord");
    std::fs::create_dir_all(&mirrord_dir).unwrap();
    std::fs::write(
        mirrord_dir.join("auth.json"),
        serde_json::to_vec_pretty(&store).unwrap(),
    )
    .unwrap();
}

/// Runs `mirrord up` on the missing-target up file with `home` as the home directory and a
/// kubeconfig that does not exist, without a terminal, as an IDE runs it. The local UI is left on,
/// so a run under a login shows that it skips it.
async fn up_without_a_kubeconfig(home: &Path) -> Output {
    let mut command = resolve_tokio_command(env!("CARGO_BIN_EXE_mirrord"));
    command
        .current_dir(home)
        // The home directory the CLI reads `~/.mirrord/auth.json` from: `HOME` on Unix,
        // `USERPROFILE` on Windows.
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("KUBECONFIG", home.join("no-such-kubeconfig"))
        .env("MIRRORD_PROGRESS_MODE", "off")
        .env_remove("MIRRORD_KUBECONFIG")
        .env_remove("MIRRORD_CI_API_KEY")
        .args(["up", "-f", MISSING_TARGET])
        .stdin(Stdio::null())
        .kill_on_drop(true);

    timeout(Duration::from_secs(30), command.output())
        .await
        .expect("`mirrord up` must finish")
        .unwrap()
}

/// Before, `mirrord up` looked a service's workload up in the cluster through the kubeconfig,
/// even under a login, so with no kubeconfig it failed with `failed to query the cluster`. Now it
/// asks the login's operator for the targets the user may list, with the login's token, and
/// reports the missing workload as it would through a kubeconfig.
#[tokio::test]
async fn up_under_a_login_looks_workloads_up_through_the_operator() {
    let home = tempfile::tempdir().unwrap();
    let (url, requests, server) = fake_operator(Targets::None).await;
    save_login(home.path(), &url, TimeDelta::hours(1));

    let output = up_without_a_kubeconfig(home.path()).await;
    server.abort();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success().not(),
        "the operator lists no `api`, so `up` must fail; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("The local UI is not started while logged in to"),
        "the local UI reads the kubeconfig, so a run under a login skips it; stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("no workload named `api` found in namespace `default`"),
        "`up` must report the missing workload, not a kubeconfig failure; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("failed to query the cluster").not(),
        "nothing may go through a kubeconfig; stderr:\n{stderr}"
    );

    let requests = requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(path, authorization)| path == TARGETS_PATH
                && authorization.as_deref() == Some(&format!("Bearer {TOKEN}"))),
        "the targets must be listed through the operator with the login's token: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|(path, _)| path.starts_with("/apis/operator.metalbear.co/")),
        "only the operator's API group answers a login: {requests:?}"
    );
}

/// An expired login stops the run once, before anything is looked up or started, with the
/// command that renews it.
#[tokio::test]
async fn up_under_an_expired_login_stops_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let (url, requests, server) = fake_operator(Targets::None).await;
    save_login(home.path(), &url, TimeDelta::hours(-1));

    let output = up_without_a_kubeconfig(home.path()).await;
    server.abort();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success().not(), "stderr:\n{stderr}");
    let words = unwrapped(&stderr);
    assert!(
        words.contains("expired") && words.contains(&format!("mirrord login --url {url}")),
        "`up` must say the login expired and how to renew it; stderr:\n{stderr}"
    );
    let requests: Vec<Value> = requests
        .lock()
        .unwrap()
        .iter()
        .map(|(path, _)| Value::String(path.clone()))
        .collect();
    assert!(
        requests.is_empty(),
        "nothing is asked of the operator: {requests:?}"
    );
}

/// A service the user may not list targets for stops the run with what to ask the admin for,
/// instead of reading as a missing workload.
#[tokio::test]
async fn up_under_a_login_names_the_permission_a_forbidden_listing_lacks() {
    let home = tempfile::tempdir().unwrap();
    let (url, _requests, server) = fake_operator(Targets::Forbidden).await;
    save_login(home.path(), &url, TimeDelta::hours(1));

    let output = up_without_a_kubeconfig(home.path()).await;
    server.abort();

    let host = url.trim_start_matches("http://");
    let words = unwrapped(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success().not(), "{words}");
    assert!(
        words.contains(&format!(
            "your login to {host} may not list targets in namespace \"default\""
        )) && words.contains("`list` on `targets` in the `operator.metalbear.co` API group"),
        "`up` must name the missing permission; stderr: {words}"
    );
}
