mod steal;

#[cfg(test)]
mod traffic {
    use std::{net::UdpSocket, path::PathBuf, time::Duration};

    use futures::Future;
    use futures_util::{stream::TryStreamExt, AsyncBufReadExt};
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::LogParams, Api, Client};
    use rstest::*;
    use tokio::{fs::File, io::AsyncWriteExt};

    use crate::utils::{
        config_dir, hostname_service, kube_client, run_exec_with_target, service,
        udp_logger_service, KubeService, CONTAINER_NAME,
    };

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn remote_dns_enabled_works(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_dns/test_remote_dns_enabled_works.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn remote_dns_lookup_google(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_dns/test_remote_dns_lookup_google.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;

        let res = process.wait().await;
        assert!(res.success());
    }

    // TODO: change outgoing TCP tests to use the same setup as in the outgoing UDP test so that
    //       they actually verify that the traffic is intercepted and forwarded (and isn't just
    //       directly sent out from the local application).
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[should_panic]
    pub async fn outgoing_traffic_single_request_ipv6(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request_ipv6.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_traffic_single_request_disabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ];
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_traffic_make_request_after_listen(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_make_request_after_listen.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_traffic_make_request_localhost(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_make_request_localhost.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
    }

    /// Currently, mirrord only intercepts and forwards outgoing udp traffic if the application
    /// binds a non-0 port and calls `connect`. This test runs with mirrord a node app that does
    /// that and verifies that mirrord intercepts and forwards the outgoing udp message.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn outgoing_traffic_udp_with_connect(
        #[future] udp_logger_service: KubeService,
        #[future] service: KubeService,
        #[future] kube_client: Client,
    ) {
        let internal_service = udp_logger_service.await; // Only reachable from withing the cluster.
        let target_service = service.await; // Impersonate a pod of this service, to reach internal.
        let kube_client = kube_client.await;
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &internal_service.namespace);
        let mut lp = LogParams {
            container: Some(String::from(CONTAINER_NAME)),
            follow: false,
            limit_bytes: None,
            pretty: false,
            previous: false,
            since_seconds: None,
            tail_lines: None,
            timestamps: false,
            since_time: None,
        };

        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client_with_connect.mjs",
            "31415",
            // Reaching service by only service name is only possible from within the cluster.
            &internal_service.name,
        ];

        // Meta-test: verify that the application cannot reach the internal service without
        // mirrord forwarding outgoing UDP traffic via the target pod.
        // If this verification fails, the test itself is invalid.
        let mirrord_no_outgoing = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command.clone(),
            &target_service.target,
            Some(&target_service.namespace),
            Some(mirrord_no_outgoing),
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(!res.success()); // Should fail because local process cannot reach service.
        let stripped_target = internal_service.target.split('/').collect::<Vec<&str>>()[1];
        let logs = pod_api.logs(stripped_target, &lp).await;
        assert_eq!(logs.unwrap(), "");

        // Run mirrord with outgoing enabled.
        let mut process = run_exec_with_target(
            node_command,
            &target_service.target,
            Some(&target_service.namespace),
            None,
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());

        // Verify that the UDP message sent by the application reached the internal service.
        lp.follow = true; // Follow log stream.

        let mut log_lines = pod_api
            .log_stream(stripped_target, &lp)
            .await
            .unwrap()
            .lines();

        let mut found = false;
        while let Some(log) = log_lines.try_next().await.unwrap() {
            println!("logged - {log}");
            if log.contains("Can I pass the test please?") {
                found = true;
                break;
            }
        }
        assert!(found, "Did not find expected log line.");
    }

    /// Very similar to [`outgoing_traffic_udp_with_connect`], but it uses the outgoing traffic
    /// filter to resolve the remote host names.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn outgoing_traffic_filter_udp_with_connect(
        config_dir: &std::path::PathBuf,
        #[future] udp_logger_service: KubeService,
        #[future] service: KubeService,
        #[future] kube_client: Client,
    ) {
        let internal_service = udp_logger_service.await; // Only reachable from withing the cluster.
        let target_service = service.await; // Impersonate a pod of this service, to reach internal.
        let kube_client = kube_client.await;
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &internal_service.namespace);
        let mut lp = LogParams {
            container: Some(String::from(CONTAINER_NAME)),
            follow: false,
            limit_bytes: None,
            pretty: false,
            previous: false,
            since_seconds: None,
            tail_lines: None,
            timestamps: false,
            since_time: None,
        };

        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client_with_connect.mjs",
            "31415",
            // Reaching service by only service name is only possible from within the cluster.
            &internal_service.name,
        ];

        // Make connections on port `31415` go through local.
        let mut config_path = config_dir.clone();
        config_path.push("outgoing_filter_local.json");

        // Meta-test: verify that the application cannot reach the internal service without
        // mirrord forwarding outgoing UDP traffic via the target pod.
        // If this verification fails, the test itself is invalid.
        let mut process = run_exec_with_target(
            node_command.clone(),
            &target_service.target,
            Some(&target_service.namespace),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;
        let res = process.wait().await;
        assert!(!res.success()); // Should fail because local process cannot reach service.
        let stripped_target = internal_service.target.split('/').collect::<Vec<&str>>()[1];
        let logs = pod_api.logs(stripped_target, &lp).await;
        assert_eq!(logs.unwrap(), "");

        // Create remote filter file with service name so we can test DNS outgoing filter.
        let service_name = internal_service.name.clone();
        let mut filter_config_path = config_dir.clone();
        let remote_config_filter = format!("outgoing_filter_remote_{service_name}.json");
        filter_config_path.push(remote_config_filter);
        let config_path = filter_config_path.clone();

        let mut remote_config_file = File::create(filter_config_path).await.unwrap();

        let remote_filter = format!(
            r#"
{{
    "feature": {{
        "network": {{
            "outgoing": {{
                "filter": {{
                    "remote": ["{service_name}"]
                }}
            }}
        }}
    }}
}}       "#
        );

        remote_config_file
            .write_all(remote_filter.as_bytes())
            .await
            .unwrap();

        // Run mirrord with outgoing enabled.
        let mut process = run_exec_with_target(
            node_command,
            &target_service.target,
            Some(&target_service.namespace),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());

        // Verify that the UDP message sent by the application reached the internal service.
        lp.follow = true; // Follow log stream.

        let mut log_lines = pod_api
            .log_stream(stripped_target, &lp)
            .await
            .unwrap()
            .lines();

        let mut found = false;
        while let Some(log) = log_lines.try_next().await.unwrap() {
            println!("logged - {log}");
            if log.contains("Can I pass the test please?") {
                found = true;
                break;
            }
        }
        assert!(found, "Did not find expected log line.");
    }

    /// Test that the process does not crash and messages are sent out normally when the
    /// application calls `connect` on a UDP socket with outgoing traffic disabled on mirrord.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn outgoing_disabled_udp(#[future] service: KubeService) {
        let service = service.await;
        // Binding specific port, because if we bind 0 then we get a  port that is bypassed by
        // mirrord and then the tested crash is not prevented by the fix but by the bypassed port.
        let socket = UdpSocket::bind("127.0.0.1:31415").unwrap();
        let port = socket.local_addr().unwrap().port().to_string();

        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client_with_connect.mjs",
            &port,
        ];
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        // Listen for UDP message directly from application.
        let mut buf = [0; 27];
        let amt = socket.recv(&mut buf).unwrap();
        assert_eq!(amt, 27);
        assert_eq!(buf, "Can I pass the test please?".as_ref()); // Sure you can.

        let res = process.wait().await;
        assert!(res.success());
    }

    pub async fn test_go(service: impl Future<Output = KubeService>, command: Vec<&str>) {
        let service = service.await;
        let mut process = run_exec_with_target(command, &service.target, None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn go19_outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let command = vec!["go-e2e-outgoing/19.go_test_app"];
        test_go(service, command).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    pub async fn go20_outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let command = vec!["go-e2e-outgoing/20.go_test_app"];
        test_go(service, command).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn go21_outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let command = vec!["go-e2e-outgoing/21.go_test_app"];
        test_go(service, command).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    pub async fn go19_dns_lookup(#[future] service: KubeService) {
        let command = vec!["go-e2e-dns/19.go_test_app"];
        test_go(service, command).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    pub async fn go20_dns_lookup(#[future] service: KubeService) {
        let command = vec!["go-e2e-dns/20.go_test_app"];
        test_go(service, command).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    pub async fn go21_dns_lookup(#[future] service: KubeService) {
        let command = vec!["go-e2e-dns/21.go_test_app"];
        test_go(service, command).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn listen_localhost(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec!["node", "node-e2e/listen/test_listen_localhost.mjs"];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    pub async fn gethostname_remote_result(#[future] hostname_service: KubeService) {
        let service = hostname_service.await;
        let node_command = vec!["python3", "-u", "python-e2e/hostname.py"];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;

        let res = process.wait().await;
        assert!(res.success());
    }

    /// Verify that when executed with mirrord an app can connect to a unix socket on the cluster.
    ///
    /// 1. Deploy to the cluster a server that listens to a pathname unix socket and echos incoming
    /// data.
    /// 2. Run with mirrord a client application that connects to that socket, sends data, verifies
    /// its echo and panics if anything went wrong
    /// 3. Verify the client app did not panic.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    pub async fn outgoing_unix_stream_pathname(
        #[future]
        #[with(
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-unix-socket-server:latest",
            "unix-echo"
        )]
        service: KubeService,
    ) {
        let service = service.await;
        let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/debug/rust-unix-socket-client")
            .to_string_lossy()
            .to_string();
        let executable = vec![app_path.as_str()];

        // Tell mirrord to connect remotely to the pathname the deployed app is listening on.
        let env = Some(vec![(
            "MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS",
            "/app/unix-socket-server.sock",
        )]);
        let mut process = run_exec_with_target(executable, &service.target, None, None, env).await;
        let res = process.wait().await;

        // The test application panics if it does not successfully connect to the socket, send data,
        // and get the same data back. So if it exits with success everything worked.
        assert!(res.success());
    }

    /// Verify that mirrord does not interfere with ignored unix sockets and connecting to a unix
    /// socket that is NOT configured to happen remotely works fine locally (testing the Bypass
    /// case of connections to unix sockets).
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn outgoing_bypassed_unix_stream_pathname(#[future] service: KubeService) {
        let service = service.await;
        let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/debug/rust-bypassed-unix-socket")
            .to_string_lossy()
            .to_string();
        let executable = vec![app_path.as_str()];

        let mut process = run_exec_with_target(executable, &service.target, None, None, None).await;
        let res = process.wait().await;

        // The test application panics if it does not successfully connect to the socket, send data,
        // and get the same data back. So if it exits with success everything worked.
        assert!(res.success());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_many_requests_enabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_many_requests.mjs",
        ];
        let mut process =
            run_exec_with_target(node_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_no_error_in_stderr().await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_many_requests_disabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_many_requests.mjs",
        ];
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_no_error_in_stderr().await;
    }
}
