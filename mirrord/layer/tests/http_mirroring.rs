#![feature(assert_matches)]
use std::{path::PathBuf, time::Duration};

use mirrord_protocol::{
    outgoing::{udp::LayerUdpOutgoing::Connect, LayerConnect},
    ClientMessage::UdpOutgoing,
};
use rstest::rstest;

mod common;

pub use common::*;
use futures::{SinkExt, StreamExt};
use mirrord_protocol::outgoing::SocketAddress;

/// For running locally, so that new developers don't have the extra step of building the go app
/// before running the tests.
#[cfg(target_os = "macos")]
#[ctor::ctor]
fn build_go_app() {
    use std::{env, path::Path, process};
    let original_dir = env::current_dir().unwrap();
    let go_app_path = Path::new("tests/apps/app_go");
    env::set_current_dir(go_app_path).unwrap();
    let output = process::Command::new("go")
        .args(vec!["build", "-o", "19"])
        .output()
        .expect("Failed to build Go test app.");
    assert!(output.status.success(), "Building Go test app failed.");
    env::set_current_dir(original_dir).unwrap();
}

/// Start a web server injected with the layer, simulate the agent, verify expected messages from
/// the layer, send tcp messages and verify in the server output that the application received them.
/// Tests the layer's communication with the agent, the bind hook, and the forwarding of mirrored
/// traffic to the application.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
async fn mirroring_with_http(
    #[values(
        Application::PythonFlaskHTTP,
        Application::PythonFastApiHTTP,
        Application::NodeHTTP
    )]
    application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer_and_port(dylib_path, vec![("MIRRORD_FILE_MODE", "local")])
        .await;

    println!("Application subscribed to port, sending tcp messages.");

    layer_connection
        .send_connection_then_data(
            "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n",
            application.get_app_port(),
        )
        .await;
    layer_connection
        .send_connection_then_data(
            "POST / HTTP/1.1\r\nHost: localhost\r\n\r\npost-data",
            application.get_app_port(),
        )
        .await;
    layer_connection
        .send_connection_then_data(
            "PUT / HTTP/1.1\r\nHost: localhost\r\n\r\nput-data",
            application.get_app_port(),
        )
        .await;
    layer_connection
        .send_connection_then_data(
            "DELETE / HTTP/1.1\r\nHost: localhost\r\n\r\ndelete-data",
            application.get_app_port(),
        )
        .await;
    if matches!(application, Application::PythonFlaskHTTP) {
        assert_eq!(
            layer_connection.codec.next().await.unwrap().unwrap(),
            UdpOutgoing(Connect(LayerConnect {
                remote_address: SocketAddress::Ip(std::net::SocketAddr::from((
                    [10, 253, 155, 219],
                    58162
                )))
            }))
        );
        // Refer: https://github.com/pallets/werkzeug/blob/main/src/werkzeug/serving.py#L640
        // We send a dummy response to connect so that werkzeug proceeds
        layer_connection
            .codec
            .send(mirrord_protocol::DaemonMessage::UdpOutgoing(
                mirrord_protocol::outgoing::udp::DaemonUdpOutgoing::Connect(Ok(
                    mirrord_protocol::outgoing::DaemonConnect {
                        connection_id: 0,
                        remote_address: SocketAddress::Ip(std::net::SocketAddr::from((
                            [10, 253, 155, 219],
                            58162,
                        ))),
                        local_address: SocketAddress::Ip(std::net::SocketAddr::from((
                            [10, 253, 155, 218],
                            58161,
                        ))),
                    },
                )),
            ))
            .await
            .unwrap();
    }
    test_process.wait().await;
    test_process.assert_stdout_contains("GET: Request completed");
    test_process.assert_stdout_contains("POST: Request completed");
    test_process.assert_stdout_contains("PUT: Request completed");
    test_process.assert_stdout_contains("DELETE: Request completed");
    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}

/// Run the http mirroring test only on MacOS, because of a known crash on Linux.
#[cfg(target_os = "macos")]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn mirroring_with_http_go(
    dylib_path: &PathBuf,
    #[values(Application::Go19HTTP, Application::Go20HTTP)] application: Application,
) {
    mirroring_with_http(application, dylib_path).await;
}
