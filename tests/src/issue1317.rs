/// Tests for mirroring existing connections.
#[cfg(test)]
mod issue1317 {
    use std::{path::PathBuf, time::Duration};

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{
        client::conn::http1::{self},
        Request,
    };
    use hyper_util::rt::TokioIo;
    use kube::Client;
    use rstest::*;
    use tokio::net::TcpStream;

    use crate::utils::{
        get_service_host_and_port, kube_client, run_exec_with_target, service, KubeService,
    };

    /// Creates a [`TcpStream`] that sends a request to `service` before mirrord is started (no
    /// agent up yet), and keeps this stream alive. Then it starts mirrord in _mirror_ mode, and
    /// sends another request that should start the sniffing/subscribe/mirror flow, even though this
    /// is not the first packet (not TCP handshake) of this connection.
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn issue1317(
        #[future]
        #[notrace]
        #[with(
            "default",
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-http-keep-alive:latest",
            "http-keep-alive"
        )]
        service: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        // Create a connection with the service before mirrord is started.
        let host = host.trim_end();
        let connection = TcpStream::connect(format!("{host}:{port}"))
            .await
            .expect(&format!("Failed connecting to {host:?}:{port:?}!"));

        let (mut request_sender, connection) =
            http1::handshake(TokioIo::new(connection)).await.unwrap();

        tokio::spawn(async move {
            if let Err(fail) = connection.await {
                panic!("Handshake [hyper] failed with {fail:#?}");
            }
        });

        // Test that we can reach the service, before mirrord is started.
        let request = Request::builder()
            .body(Full::new(Bytes::from(format!("GET 1"))))
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
        let executable = vec![app_path.as_str()];
        let mut process = run_exec_with_target(executable, &service.target, None, None, None).await;

        process
            .wait_for_line(Duration::from_secs(120), "daemon subscribed")
            .await;

        // Now this request should be mirrored back to us, even though mirrord started sniffing
        // mid-session.
        let request = Request::builder()
            .body(Full::new(Bytes::from(format!("GET 2"))))
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

        let request = Request::builder()
            .body(Full::new(Bytes::from(format!("EXIT"))))
            .unwrap();
        let response = request_sender
            .send_request(request)
            .await
            .expect("3rd request ends the program!");
        assert!(response.status().is_success());

        drop(request_sender);
        process.wait_assert_success().await;
    }
}
