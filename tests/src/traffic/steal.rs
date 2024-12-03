#![allow(dead_code, unused)]
#[cfg(test)]
mod steal_tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{SocketAddr, TcpStream},
        path::Path,
        time::Duration,
    };

    use futures_util::{SinkExt, StreamExt};
    use kube::Client;
    use reqwest::{header::HeaderMap, Url};
    use rstest::*;
    use tokio::time::sleep;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message},
    };

    use crate::utils::{
        config_dir, get_service_host_and_port, get_service_url, http2_service, kube_client,
        send_request, send_requests, service, tcp_echo_service, websocket_service, Application,
        KubeService,
    };

    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn steal_http_traffic(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];

        if cfg!(feature = "ephemeral") {
            flags.extend(["-e"].into_iter());
        }

        let mut process = application
            .run(&service.target, Some(&service.namespace), Some(flags), None)
            .await;

        process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;
        send_requests(&url, true, Default::default()).await;
        tokio::time::timeout(Duration::from_secs(40), process.wait())
            .await
            .unwrap();

        application.assert(&process).await;
    }

    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn steal_http_ipv6_traffic(
        #[future] service: KubeService,
        #[future] kube_client: Client,
    ) {
        let application = Application::PythonFastApiHTTPIPv6;
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];

        if cfg!(feature = "ephemeral") {
            flags.extend(["-e"].into_iter());
        }

        let mut process = application
            .run(&service.target, Some(&service.namespace), Some(flags), None)
            .await;

        process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;
        send_requests(&url, true, Default::default()).await;
        tokio::time::timeout(Duration::from_secs(40), process.wait())
            .await
            .unwrap();

        application.assert(&process).await;
    }

    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn steal_http_traffic_with_flush_connections(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];

        if cfg!(feature = "ephemeral") {
            flags.extend(["-e"].into_iter());
        }

        let mut process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "true")]),
            )
            .await;

        process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;
        send_requests(&url, true, Default::default()).await;
        tokio::time::timeout(Duration::from_secs(40), process.wait())
            .await
            .unwrap();

        application.assert(&process).await;
    }

    /// Test the app continues running with mirrord and traffic is no longer stolen after the app
    /// closes a socket.
    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn close_socket(#[future] service: KubeService, #[future] kube_client: Client) {
        let application = Application::PythonCloseSocket;
        // Start the test app with mirrord
        let service = service.await;
        let kube_client = kube_client.await;
        let mut flags = vec!["--steal"];

        if cfg!(feature = "ephemeral") {
            flags.extend(["-e"].into_iter());
        }

        let mut process = application
            .run(&service.target, Some(&service.namespace), Some(flags), None)
            .await;

        // Verify that we hooked the socket operations and the agent started stealing.
        process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        // Wait for the test app to close the socket and tell us about it.
        process
            .wait_for_line(Duration::from_secs(40), "Closed socket")
            .await;

        // Flake-proofing the test:
        // If we connect to the service between the time the test application had closed its socket
        // and the agent was informed about it, our connection would still get stolen at first, but
        // then reset without response data once the agent learns the socket was closed.
        //
        // To try to prevent this from happening in the test, we wait after the app closes the
        // socket, to make sure mirrord has time to handle the close, before we send a request.
        // Because things could go slow on GitHub Actions, we wait for 3 full seconds.
        //
        // In order to make this test deterministic, we could make the agent send a confirmation
        // once it's done handling the `PortUnsubscribe`, but that would require adding a new
        // message to the protocol, which we only do on major protocol version bumps.
        sleep(Duration::from_secs(3)).await;

        // Send HTTP request and verify it is handled by the REMOTE app - NOT STOLEN.
        let url = get_service_url(kube_client.clone(), &service).await;
        let client = reqwest::Client::new();
        let req_builder = client.get(url);
        eprintln!("Sending request to remote service");
        send_request(
            req_builder,
            Some("OK - GET: Request completed\n"),
            Default::default(),
        )
        .await;

        process
            .write_to_stdin(b"Hey test app, please stop running and just exit successfuly.\n")
            .await;

        eprintln!("Waiting for test app to exit successfully.");
        process.wait_assert_success().await;
    }

    /// Test that when we close a listening socket any connected sockets accepted from that sockets
    /// continue working, and that once the connected sockets are closed, we stop stealing.
    ///
    /// 1. Test app creates socket.
    /// 2. Test app binds, listens, and accepts a connection.
    /// 3. This test connects to the test app.
    /// 4. The test app closes the original socket (but not the connection's socket).
    /// 5. This test sends data to the test app.
    /// 6. The test app sends the data back, in upper case.
    /// 7. The test app closes also the connection's socket.
    /// 8. This test tells the test app to exit.
    /// 9. This test sends a request to the service and verifies it's not stolen.
    ///
    /// This test is ignored: it's a
    /// [known issue](https://github.com/metalbear-co/mirrord/issues/1575): when the user
    /// application closes a socket, we stop stealing existing connections.
    #[ignore]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn close_socket_keep_connection(
        #[future] service: KubeService,
        #[future] kube_client: Client,
    ) {
        let application = Application::PythonCloseSocketKeepConnection;
        // Start the test app with mirrord
        let service = service.await;
        let kube_client = kube_client.await;
        let mut flags = vec!["--steal"];

        if cfg!(feature = "ephemeral") {
            flags.extend(["-e"].into_iter());
        }

        let mut process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![
                    ("RUST_LOG", "mirrord=trace"),
                    ("MIRRORD_AGENT_RUST_LOG", "mirrord=trace"),
                    ("MIRRORD_AGENT_TTL", "30"),
                ]),
            )
            .await;

        let (addr, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        // Wait for the app to start listening for stolen data before connecting.
        process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        let mut tcp_stream = TcpStream::connect((addr, port as u16)).unwrap();

        // Wait for the test app to close the socket and tell us about it.
        process
            .wait_for_line(Duration::from_secs(40), "Closed socket.")
            .await;

        const DATA: &[u8; 16] = b"upper me please\n";

        tcp_stream.write_all(DATA).unwrap();

        let mut response = [0u8; DATA.len()];
        tcp_stream.read_exact(&mut response).unwrap();

        process
            .write_to_stdin(b"Hey test app, please stop running and just exit successfuly.\n")
            .await;

        assert_eq!(
            response,
            String::from_utf8_lossy(DATA).to_uppercase().as_bytes()
        );

        eprintln!("Waiting for test app to exit successfully.");
        process.wait_assert_success().await;

        // Send HTTP request and verify it is handled by the REMOTE app - NOT STOLEN.
        let url = get_service_url(kube_client.clone(), &service).await;
        let client = reqwest::Client::new();
        let req_builder = client.get(url);
        eprintln!("Sending request to remote service");
        send_request(
            req_builder,
            Some("OK - GET: Request completed\n"),
            Default::default(),
        )
        .await;
    }

    /// To run on mac, first build universal binary: (from repo root) `scripts/build_fat_mac.sh`
    /// then run test with MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    async fn filter_with_single_client_and_only_matching_requests(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let flags = vec!["--steal"];

        let mut client = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        client
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap());
        send_requests(&url, true, headers).await;

        let _ = tokio::time::timeout(Duration::from_secs(40), client.child.wait())
            .await
            .unwrap();

        application.assert(&client).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    async fn filter_with_single_client_and_only_matching_requests_new(
        config_dir: &Path,
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::NodeHTTP)] application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;

        let mut config_path = config_dir.to_path_buf();
        config_path.push("http_filter_header.json");

        let mut client = application
            .run(
                &service.target,
                Some(&service.namespace),
                None,
                Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
            )
            .await;

        client
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap());
        send_requests(&url, true, headers).await;

        let _ = tokio::time::timeout(Duration::from_secs(40), client.child.wait())
            .await
            .unwrap();

        application.assert(&client).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    async fn filter_with_single_client_requests_by_path(
        config_dir: &Path,
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::NodeHTTP)] application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;

        let mut config_path = config_dir.to_path_buf();
        config_path.push("http_filter_path.json");

        let mut client = application
            .run(
                &service.target,
                Some(&service.namespace),
                None,
                Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
            )
            .await;

        client
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        let headers = HeaderMap::default();
        // Send a GET that should go through to remote
        let req_client = reqwest::Client::new();
        let req_builder = req_client.get(&url);
        send_request(
            req_builder,
            Some("OK - GET: Request completed\n"),
            headers.clone(),
        )
        .await;

        // Send a DELETE that should match and cause the local app to return specific response
        // and also make the process quit.
        let req_client = reqwest::Client::new();
        let mut match_url = Url::parse(&url).unwrap();
        match_url.set_path("/api/v1");
        let req_builder = req_client.delete(match_url);
        send_request(req_builder, Some("DELETEV1"), headers.clone()).await;

        let _ = tokio::time::timeout(Duration::from_secs(40), client.child.wait())
            .await
            .unwrap();

        application.assert(&client).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    async fn test_filter_with_single_client_and_only_matching_requests_http2(
        #[future] http2_service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::NodeHTTP2)] application: Application,
    ) {
        let service = http2_service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let flags = vec!["--steal", "--fs-mode=local"];

        let mut mirrored_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrored_process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

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
                Some("<h1>Hello HTTP/2: <b>from local app</b></h1>"),
                headers.clone(),
            )
            .await;

            let delete_builder = client.delete(&url);
            let mut headers = HeaderMap::default();
            headers.insert("x-filter", "yes".parse().unwrap());
            send_request(
                delete_builder,
                Some("<h1>Hello HTTP/2: <b>from local app</b></h1>"),
                headers.clone(),
            )
            .await;
        }

        tokio::time::timeout(Duration::from_secs(40), mirrored_process.wait())
            .await
            .expect("mirrored_process failed!");

        application.assert(&mirrored_process).await;
    }

    /// To run on mac, first build universal binary: (from repo root) `scripts/build_fat_mac.sh`
    /// then run test with MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    async fn filter_with_single_client_and_some_matching_requests(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;

        let flags = vec!["--steal"];

        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

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

        tokio::time::timeout(Duration::from_secs(10), mirrorded_process.wait())
            .await
            .unwrap();

        application.assert(&mirrorded_process).await;
    }

    /// Test the case where running with `steal` set and an http header filter, but getting a
    /// connection of an unsupported protocol.
    /// We verify that the traffic is forwarded to- and handled by the deployed app, and the local
    /// app does not see the traffic.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    async fn complete_passthrough(
        #[future] tcp_echo_service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::PythonFastApiHTTP, Application::NodeHTTP)] application: Application,
        #[values("THIS IS NOT HTTP!\n", "short.\n")] tcp_data: &str,
    ) {
        let service = tcp_echo_service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        let flags = vec!["--steal"];

        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        let addr = SocketAddr::new(host.trim().parse().unwrap(), port as u16);
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(tcp_data.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        println!("Got response: {buf}");
        // replace "remote: " with empty string, since the response can be split into frames
        // and we just need assert the final response
        buf = buf.replace("remote: ", "");
        assert_eq!(&buf, tcp_data); // The correct data was sent there and back.

        // Verify the data was passed through and nothing was sent to the local app.
        let stdout_after = mirrorded_process.get_stdout().await;
        assert!(!stdout_after.contains("LOCAL APP GOT DATA"));

        // Send a DELETE that should be matched and thus stolen, closing the app.
        let url = get_service_url(kube_client.clone(), &service).await;
        let client = reqwest::Client::new();
        let req_builder = client.delete(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap()); // header DOES match.
        send_request(req_builder, Some("DELETE"), headers.clone()).await;

        tokio::time::timeout(Duration::from_secs(10), mirrorded_process.wait())
            .await
            .unwrap();

        application.assert(&mirrorded_process).await;
    }

    /// Test the case where we're running with `steal` set and an http header filter, the target
    /// gets an HTTP upgrade request, but the request does not match the filter and should not
    /// reach the local app.
    ///
    /// We verify that the traffic is handled by the deployed app, and the local
    /// app does not see the traffic.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn websocket_upgrade_no_filter_match(
        #[future] websocket_service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::PythonFastApiHTTP, Application::NodeHTTP)] application: Application,
        #[values(
            "Hello, websocket!\n".to_string(),
            "websocket\n".to_string()
        )]
        write_data: String,
    ) {
        let service = websocket_service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        let flags = vec!["--steal"];

        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

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
        let received_data = read_message
            .strip_prefix("remote: ")
            .expect("data should be passed through to the remote app");
        assert_eq!(
            received_data, write_data,
            "same data should be sent there and back"
        );

        // Verify the data was passed through and nothing was sent to the local app.
        let stdout_after = mirrorded_process.get_stdout().await;
        assert!(!stdout_after.contains("LOCAL APP GOT DATA"));

        // Send a DELETE that should be matched and thus stolen, closing the app.
        let url = get_service_url(kube_client.clone(), &service).await;
        let client = reqwest::Client::new();
        let req_builder = client.delete(&url);
        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap()); // header DOES match.
        send_request(req_builder, Some("DELETE"), headers.clone()).await;

        tokio::time::timeout(Duration::from_secs(10), mirrorded_process.wait())
            .await
            .unwrap();

        application.assert(&mirrorded_process).await;
    }

    /// Test the case where we're running with `steal` set and an http header filter, the target
    /// gets an HTTP upgrade request, the request matches the filter and the whole websocket
    /// connection should be handled by the local app.
    ///
    /// We verify that the traffic is handled by the local app.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn websocket_upgrade_filter_match(
        #[future] websocket_service: KubeService,
        #[future] kube_client: Client,
        #[values(Application::RustWebsockets)] application: Application,
    ) {
        let service = websocket_service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(vec!["--steal"]),
                Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
            )
            .await;

        mirrorded_process
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        // Create a websocket connection to test the HTTP upgrade steal.
        // Add a header so that the request matches the filter.
        let mut request = format!("ws://{}:{port}", host.trim())
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .append("x-filter", "yes".try_into().unwrap());
        let (mut stream, _) = connect_async(request)
            .await
            .expect("failed to create connection");

        let messages = [
            Message::Text("local: hello_1".to_string()),
            Message::Binary("local: hello_2".as_bytes().to_vec()),
        ];
        for message in &messages {
            stream
                .send(message.clone())
                .await
                .expect("failed to send message");
            loop {
                let response = stream
                    .next()
                    .await
                    .expect("connection broke")
                    .expect("failed to read message");
                match response {
                    Message::Ping(data) => stream
                        .send(Message::Pong(data))
                        .await
                        .expect("failed to send message"),
                    response if &response == message => break,
                    other => panic!("unexpected message received: {other:?}"),
                }
            }
        }

        stream
            .close(None)
            .await
            .expect("failed to close connection");

        let status = mirrorded_process.wait().await;
        assert!(status.success(), "test process failed");
    }
}
