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

    use crate::utils::{
        get_service_host_and_port, get_service_url, kube_client, send_request, send_requests,
        service, tcp_echo_service, Agent, Application, KubeService,
    };

    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_steal_http_traffic(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
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
        agent.flag().map(|flag| flags.extend(flag));
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

    /// To run on mac, first build universal binary: (from repo root) `scripts/build_fat_mac.sh`
    /// then run test with MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(45))]
    async fn test_filter_with_single_client_and_only_matching_requests(
        #[future] service: KubeService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
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
        agent.flag().map(|flag| flags.extend(flag));

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
        agent.flag().map(|flag| flags.extend(flag));
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

        // Send a DELETE that should not be matched and thus not stolen.
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
        #[values("THIS IS NOT HTTP!\n", "short.\n")] tcp_data: &str,
    ) {
        let application = Application::NodeTcpEcho;
        let service = tcp_echo_service.await;
        let kube_client = kube_client.await;
        let (host, port) = get_service_host_and_port(kube_client.clone(), &service).await;

        let mut flags = vec!["--steal"];
        agent.flag().map(|flag| flags.extend(flag));
        let mut mirrorded_process = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![
                    ("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes"),
                    // set time out to 1 to avoid two agents conflict
                    ("MIRRORD_AGENT_COMMUNICATION_TIMEOUT", "3"),
                ]),
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

        mirrorded_process.child.kill().await.unwrap();

        // Wait for agent to exit.
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Now do a meta-test to see that with this setup but without the http filter the data does
        // reach the local app.

        let mut flags = vec!["--steal"];
        agent.flag().map(|flag| flags.extend(flag));
        let mut mirrorded_process = application
            .run(&service.target, Some(&service.namespace), Some(flags), None)
            .await;

        mirrorded_process.wait_for_line(Duration::from_secs(15), "daemon subscribed");

        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write(tcp_data.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        assert_eq!(&buf[..7], "local: "); // The data was stolen to local app.
        assert_eq!(&buf[7..], tcp_data); // The correct data was sent there and back.

        // Verify the data was sent to the local app.
        let stdout_after = mirrorded_process.get_stdout();
        assert!(stdout_after.contains("LOCAL APP GOT DATA"));
        mirrorded_process.child.kill().await.unwrap();
    }
}
