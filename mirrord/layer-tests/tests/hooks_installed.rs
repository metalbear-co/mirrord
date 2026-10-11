#![cfg(target_family = "unix")]

//! Checks that the layer tells the intproxy which of its hooks it installed.

use std::time::Duration;

use mirrord_intproxy_protocol::LayerToProxyMessage;
use mirrord_layer_tests::fake_intproxy::FakeIntProxy;

mod common;

pub use common::*;

const TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn layer_reports_installed_hooks() {
    let mut intproxy = FakeIntProxy::new().await;
    // With local file operations the layer has nothing else to ask the fake intproxy, which does
    // not answer requests.
    let env = get_env(intproxy.address(), vec![("MIRRORD_FILE_MODE", "local")]);
    let application = Application::DynamicApp(
        Application::get_python3_executable().await,
        vec!["-c".to_owned(), "pass".to_owned()],
    );
    let _process = application.get_test_process(env, None).await;

    let report = 'report: loop {
        // `python3` can be a wrapper that loads the layer and then calls `exec`, in which case
        // more than one layer connects.
        let mut layer = intproxy
            .try_accept(TIMEOUT)
            .await
            .expect("no layer reported its hooks");

        while let Some(message) = tokio::time::timeout(TIMEOUT, layer.recv())
            .await
            .expect("the layer sent nothing")
        {
            if let LayerToProxyMessage::HooksInstalled(report) = message.inner {
                break 'report report;
            }
        }
    };

    // `close` exists on every platform and is hooked regardless of configuration.
    assert!(
        report.installed.iter().any(|symbol| symbol == "close"),
        "{report:?}"
    );
    assert!(
        report
            .failed
            .iter()
            .all(|symbol| !report.installed.contains(symbol)),
        "{report:?}"
    );
}
