#[cfg(test)]
mod steal {
    use std::{
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpStream},
        time::Duration,
    };

    use kube::Client;
    use reqwest::header::HeaderMap;
    use rstest::*;
    use tokio_tungstenite::connect_async;

    use crate::utils::{
        get_service_host_and_port, get_service_url, kube_client, send_request, send_requests,
        service, tcp_echo_service, websocket_service, Agent, Application, KubeService,
    };

    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_steal_http_traffic(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            // Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }
        let mut process = application
            .run(&service.target, Some(&service.namespace), Some(flags), None)
            .await;

        process.wait_for_line(Duration::from_secs(40), "daemon subscribed");
        send_requests(&url, true, Default::default()).await;
        tokio::time::timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_steal_http_traffic_with_flush_connections(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            // Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }
        let mut process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "true")]),
            )
            .await;

        process.wait_for_line(Duration::from_secs(40), "daemon subscribed");
        send_requests(&url, true, Default::default()).await;
        tokio::time::timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    /// To run on mac, first build universal binary: (from repo root) `scripts/build_fat_mac.sh`
    /// then run test with MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(45))]
    async fn test_filter_with_single_client_and_only_matching_requests(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            // Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }

        let mut client = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        client.wait_for_line(Duration::from_secs(40), "daemon subscribed");

        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap());
        send_requests(&url, true, headers).await;

        tokio::time::timeout(Duration::from_secs(40), client.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&client);
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_filter_with_single_client_and_only_matching_requests_http2(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::NodeHTTP2)] application: Application,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal", "--fs-mode=local"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }

        let mut mirrored_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrored_process.wait_for_line(Duration::from_secs(40), "daemon subscribed");

        // Send a GET that should be matched and stolen.
        // And a DELETE that closes the app.
        {
            let client = reqwest::Client::builder()
                .http2_prior_knowledge()
                .build()
                .unwrap();

            let get_builder = client.get(&url);
            let mut headers = HeaderMap::default();
            headers.insert("x-filter", "yes".parse().unwrap());
            send_request(
                get_builder,
                Some("<h1>Hello World: <b>from local app</b></h1>"),
                headers.clone(),
            )
            .await;

            let delete_builder = client.delete(&url);
            let mut headers = HeaderMap::default();
            headers.insert("x-filter", "yes".parse().unwrap());
            send_request(
                delete_builder,
                Some("<h1>Hello World: <b>from local app</b></h1>"),
                headers.clone(),
            )
            .await;
        }

        tokio::time::timeout(Duration::from_secs(120), mirrored_process.child.wait())
            .await
            .expect("Timed out waiting for mirrored_process!")
            .expect("mirrored_process failed!");

        application.assert(&mirrored_process);
    }

    /// To run on mac, first build universal binary: (from repo root) `scripts/build_fat_mac.sh`
    /// then run test with MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(45))]
    async fn test_filter_with_single_client_and_some_matching_requests(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            // Application::PythonFlaskHTTP, // TODO?
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;

        let mut flags = vec!["--steal"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }
        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process.wait_for_line(Duration::from_secs(40), "daemon subscribed");

        // Send a GET that should be matched and stolen.
        let client = reqwest::Client::new();
        let req_builder = client.get(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap());
        send_request(req_builder, Some("GET"), headers.clone()).await;

        // Send a DELETE that should not be matched and thus not stolen.
        let client = reqwest::Client::new();
        let req_builder = client.delete(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "no".parse().unwrap()); // header does NOT match.
        send_request(req_builder, None, headers.clone()).await;

        // Since the app exits on DELETE, if there's a bug and the DELETE was stolen even though it
        // was not supposed to, the app would now exit and the next request would fail.

        // Send a DELETE that should be matched and thus stolen, closing the app.
        let client = reqwest::Client::new();
        let req_builder = client.delete(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap()); // header DOES match.

        send_request(req_builder, Some("DELETE"), headers.clone()).await;

        tokio::time::timeout(Duration::from_secs(10), mirrorded_process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&mirrorded_process);
    }

    /// Test the case where running with `steal` set and an http header filter, but getting a
    /// connection of an unsupported protocol.
    /// We verify that the traffic is forwarded to- and handled by the deployed app, and the local
    /// app does not see the traffic.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn test_complete_passthrough(
        #[future] tcp_echo_service: KubeService,
        #[future] kube_client: Client,
        #[values(Agent::Job)] agent: Agent,
        #[values(Application::PythonFastApiHTTP, Application::NodeHTTP)] application: Application,
        #[values("THIS IS NOT HTTP!\n", "short.\n")] tcp_data: &str,
    ) {
        let service = tcp_echo_service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        let mut flags = vec!["--steal"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }
        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process.wait_for_line(Duration::from_secs(15), "daemon subscribed");

        let addr = SocketAddr::new(host.trim().parse().unwrap(), port as u16);
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write(tcp_data.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        println!("Got response: {buf}");
        assert_eq!(&buf[..8], "remote: "); // The data was passed through to remote app.
        assert_eq!(&buf[8..], tcp_data); // The correct data was sent there and back.

        // Verify the data was passed through and nothing was sent to the local app.
        let stdout_after = mirrorded_process.get_stdout();
        assert!(!stdout_after.contains("LOCAL APP GOT DATA"));

        // Send a DELETE that should be matched and thus stolen, closing the app.
        let url = get_service_url(kube_client.clone(), &service).await;
        let client = reqwest::Client::new();
        let req_builder = client.delete(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap()); // header DOES match.
        send_request(req_builder, Some("DELETE"), headers.clone()).await;

        tokio::time::timeout(Duration::from_secs(10), mirrorded_process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&mirrorded_process);
    }

    /// Test the case where running with `steal` set and an http header filter, we get an HTTP
    /// upgrade request, and this should not reach the local app.
    ///
    /// We verify that the traffic is forwarded to- and handled by the deployed app, and the local
    /// app does not see the traffic.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn test_websocket_upgrade(
        #[future] websocket_service: KubeService,
        #[future] kube_client: Client,
        #[values(Agent::Job)] agent: Agent,
        #[values(Application::PythonFastApiHTTP, Application::NodeHTTP)] application: Application,
        #[values(
            "Hello, websocket!\n".to_string(),
            "websocket\n".to_string()
        )]
        write_data: String,
    ) {
        use futures_util::{SinkExt, StreamExt};

        let service = websocket_service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        let mut flags = vec!["--steal"];
        if let Some(flag) = agent.flag() {
            flags.extend(flag)
        }
        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process.wait_for_line(Duration::from_secs(20), "daemon subscribed");

        // Create a websocket connection to test the HTTP upgrade bypass.
        let host = host.trim();
        let (ws_stream, _) = connect_async(format!("ws://{host}:{port}"))
            .await
            .expect("Failed websocket connection!");

        let (mut ws_write, mut ws_read) = ws_stream.split();

        ws_write
            .send(write_data.clone().into())
            .await
            .expect("Failed writing to websocket!");

        let read_message = ws_read
            .next()
            .await
            .expect("No message!")
            .expect("Reading message failed!")
            .into_text()
            .expect("Read message was not text!");

        println!("Got response: {read_message}");
        assert_eq!(&read_message[..8], "remote: "); // The data was passed through to remote app.
        assert_eq!(&read_message[8..], write_data); // The correct data was sent there and back.

        // Verify the data was passed through and nothing was sent to the local app.
        let stdout_after = mirrorded_process.get_stdout();
        assert!(!stdout_after.contains("LOCAL APP GOT DATA"));

        // Send a DELETE that should be matched and thus stolen, closing the app.
        let url = get_service_url(kube_client.clone(), &service).await;
        let client = reqwest::Client::new();
        let req_builder = client.delete(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap()); // header DOES match.
        send_request(req_builder, Some("DELETE"), headers.clone()).await;

        tokio::time::timeout(Duration::from_secs(10), mirrorded_process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&mirrorded_process);
    }
}
