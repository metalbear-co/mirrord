/// Tests for mirroring existing connections.
#[cfg(test)]
mod issue1317_tests {
    use std::{io::Write, path::PathBuf, time::Duration};

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{
        client::conn::http1::{self},
        Request,
    };
    use hyper_util::rt::TokioIo;
    use kube::Client;
    use rstest::*;
    use tempfile::NamedTempFile;
    use tokio::net::TcpStream;

    use crate::utils::{
        kube_client, kube_service::KubeService, port_forwarder::PortForwarder,
        run_command::run_exec_with_target, services::basic_service,
    };

    /// Creates a [`TcpStream`] that sends a request to `service` before mirrord is started (no
    /// agent up yet), and keeps this stream alive. Then it starts mirrord in _mirror_ mode, and
    /// sends another request that should start the sniffing/subscribe/mirror flow, even though this
    /// is not the first packet (not TCP handshake) of this connection.
    ///
    /// # Deprecation notice
    ///
    /// This test is not compatible with the new mirroring mode (passthrough mirroring).
    /// It should be removed once the old mirroring mode (raw socket sniffing) is removed.
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(feature = "job"), ignore)]
    async fn issue1317(
        #[future]
        #[notrace]
        #[with(
            "default",
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-http-keep-alive:latest",
            "http-keep-alive"
        )]
        basic_service: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
    ) {
        let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
        let config = serde_json::json!({
            "agent": {
                "passthrough_mirroring": false
            },
        });
        config_file
            .as_file_mut()
            .write_all(config.to_string().as_bytes())
            .unwrap();

        let service = basic_service.await;
        let kube_client = kube_client.await;
        let portforwarder = PortForwarder::new(
            kube_client.clone(),
            &service.pod_name,
            &service.namespace,
            80,
        )
        .await;

        // Create a connection with the service before mirrord is started.
        let connection = TcpStream::connect(portforwarder.address()).await.unwrap();

        let (mut request_sender, connection) =
            http1::handshake(TokioIo::new(connection)).await.unwrap();

        let conn_handle = tokio::spawn(connection);

        // Test that we can reach the service, before mirrord is started.
        let request = Request::builder()
            .body(Full::new(Bytes::from("GET 1".to_string())))
            .unwrap();
        let response = request_sender
            .send_request(request)
            .await
            .expect("1st request failed!");
        assert!(response.status().is_success());

        let body = response.into_body().collect().await.unwrap();
        assert!(String::from_utf8_lossy(&body.to_bytes()[..]).contains("Echo [remote]"));

        // Started mirrord.
        let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/debug/issue1317")
            .to_string_lossy()
            .to_string();
        let executable = vec![app_path];
        let mut process = run_exec_with_target(
            executable,
            &service.pod_container_target(),
            None,
            Some(vec!["-f", config_file.path().to_str().unwrap()]),
            None,
        )
        .await;

        process
            .wait_for_line(Duration::from_secs(120), "daemon subscribed")
            .await;

        // Now this request should be mirrored back to us, even though mirrord started sniffing
        // mid-session.
        let request = Request::builder()
            .body(Full::new(Bytes::from("GET 2".to_string())))
            .unwrap();
        let response = request_sender
            .send_request(request)
            .await
            .expect("2nd request failed!");
        assert!(response.status().is_success());

        let body = response.into_body().collect().await.unwrap();
        assert!(String::from_utf8_lossy(&body.to_bytes()[..]).contains("Echo [remote]"));

        process
            .wait_for_line(Duration::from_secs(60), "Echo [local]: GET 2")
            .await;

        // The last request sends `"EXIT"` in the body, and this triggers a `process::exit(0)` on
        // the local process.
        let request = Request::builder()
            .body(Full::new(Bytes::from("EXIT".to_string())))
            .unwrap();
        let response = request_sender
            .send_request(request)
            .await
            .expect("3rd request ends the program!");
        assert!(response.status().is_success());

        process.wait_assert_success().await;

        std::mem::drop(request_sender);
        conn_handle.await.unwrap().unwrap();
    }
}
