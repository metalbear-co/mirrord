//! `mirrord up` started without a terminal, the way IDE extensions start it.

use std::{ops::Not, process::Stdio, time::Duration};

use axum::{Json, Router};
use mirrord_command::resolve_tokio_command;
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};

/// The Windows process creation flag that gives the child a console no one sees.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const MISSING_TARGET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/testdata/up-missing-target.yaml"
);

/// Answers every request as a cluster with no workloads.
async fn empty_list() -> Json<Value> {
    Json(json!({"apiVersion": "v1", "kind": "List", "metadata": {}, "items": []}))
}

/// A service with no `target.path` and no workload named after it fails `mirrord up` at once
/// when there's no terminal, instead of waiting on a prompt nobody can answer.
#[tokio::test]
async fn up_without_a_terminal_fails_on_a_missing_target_instead_of_prompting() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(empty_list))
            .await
            .unwrap()
    });

    let kubeconfig = directory.path().join("kubeconfig.json");
    std::fs::write(
        &kubeconfig,
        serde_json::to_vec(&json!({
            "apiVersion": "v1",
            "kind": "Config",
            "clusters": [{"name": "fixture", "cluster": {"server": format!("http://{address}")}}],
            "users": [{"name": "fixture", "user": {}}],
            "contexts": [{"name": "fixture", "context": {"cluster": "fixture", "user": "fixture", "namespace": "default"}}],
            "current-context": "fixture"
        }))
        .unwrap(),
    )
    .unwrap();

    let mut command = resolve_tokio_command(env!("CARGO_BIN_EXE_mirrord"));
    command
        .current_dir(directory.path())
        .env("KUBECONFIG", &kubeconfig)
        .env("MIRRORD_PROGRESS_MODE", "off")
        .env_remove("MIRRORD_CI_API_KEY")
        .args([
            "up",
            "-f",
            MISSING_TARGET,
            "--no-ui",
            "--context",
            "fixture",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    // An IDE's `up` has a console that no one sees, and a prompt would wait on it.
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = timeout(Duration::from_secs(30), command.output())
        .await
        .expect("`mirrord up` without a terminal must not wait on a prompt")
        .unwrap();
    server.abort();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success().not(),
        "the service has no target, so `up` must fail; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("no workload named `api` found in namespace `default`"),
        "`up` must name the service whose target is missing; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("What would you like to do?").not(),
        "no one can answer a prompt without a terminal; stderr:\n{stderr}"
    );
}
