use std::time::Duration;

use rstest::rstest;
use tokio::net::TcpListener;

mod common;
pub use common::*;

/// Ensures that `mirrord exec` extracts the layer when `MIRRORD_LAYER_FILE` is not set.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn exec_extracts_layer_without_env() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let env = get_env(address, vec![("MIRRORD_FILE_MODE", "local")]);

    let env_pairs: Vec<(String, String)> = env.into_iter().collect();
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let application = Application::DynamicApp(
        "python3".to_string(),
        vec!["-c".to_string(), "\"print('hi')\"".to_string()],
    );
    let executable = application.get_executable().await;
    let cmdline: Vec<String> = [executable]
        .into_iter()
        .chain(application.get_args())
        .collect();

    let mut process = run_exec(
        cmdline,
        None,
        None,
        None,
        Some(env_refs),
        Some(&["MIRRORD_LAYER_FILE"]),
    )
    .await;

    let mut intproxy = TestIntProxy::new(listener, None).await;
    let intproxy_task = tokio::spawn(async move {
        while let Some(message) = intproxy.try_recv().await {
            panic!("unexpected message from layer: {message:?}");
        }
    });

    process.wait_assert_success().await;
    process.assert_stdout_contains("hi").await;
    process
        .assert_stderr_contains("MIRRORD_LAYER_FILE not set, extracting library from binary")
        .await;
    intproxy_task.await.unwrap();
}
