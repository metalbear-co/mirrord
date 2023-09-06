#[cfg(test)]
mod issue1317 {
    use std::time::Duration;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{
        client::conn::http1::{self},
        Request,
    };
    use hyper_util::rt::TokioIo;
    use kube::Client;
    use rstest::*;
    use tokio::{net::TcpStream, time::timeout};

    use crate::utils::{
        get_service_host_and_port, kube_client, service, Agent, Application, KubeService,
    };

    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn issue1317(
        #[future]
        #[notrace]
        service: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(Application::RustIssue1317)] application: Application,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        // Create a connection with the service before mirrord is started.
        let connection = TcpStream::connect(format!("{host}:{port}"))
            .await
            .expect("Failed connecting to {host}:{port}!");

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

        let mut process = application
            .run(
                &service.target,
                Some(&service.namespace),
                agent.flag(),
                None,
            )
            .await;
        process
            .wait_for_line(Duration::from_secs(120), "daemon subscribed")
            .await;

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
            .wait_for_line(Duration::from_secs(10), "Echo [local]: GET 2")
            .await;

        timeout(Duration::from_secs(40), process.wait())
            .await
            .unwrap();

        application.assert(&process).await;
    }
}
