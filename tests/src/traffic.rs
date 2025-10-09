mod mirror;
mod steal;

#[cfg(test)]
mod traffic_tests {
    use std::{net::UdpSocket, ops::Not, path::PathBuf, time::Duration};

    use futures::{stream, StreamExt};
    use futures_util::{stream::TryStreamExt, AsyncBufReadExt};
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::LogParams, Api, Client};
    use rstest::*;

    use crate::utils::{
        application::{Application, GoVersion},
        ipv6::ipv6_service,
        kube_client,
        kube_service::KubeService,
        run_command::run_exec_with_target,
        services::{basic_service, hostname_service, udp_logger_service},
        CONTAINER_NAME,
    };

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn remote_dns_enabled_works(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/remote_dns/test_remote_dns_enabled_works.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn remote_dns_lookup_google(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/remote_dns/test_remote_dns_lookup_google.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    // TODO: change outgoing TCP tests to use the same setup as in the outgoing UDP test so that
    //       they actually verify that the traffic is intercepted and forwarded (and isn't just
    //       directly sent out from the local application).
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_traffic_single_request_enabled(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[should_panic]
    pub async fn outgoing_traffic_single_request_ipv6(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request_ipv6.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    pub async fn outgoing_traffic_single_request_ipv6_enabled(#[future] ipv6_service: KubeService) {
        let service = ipv6_service.await;
        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request_ipv6.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            Some(vec![("MIRRORD_ENABLE_IPV6", "true")]),
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    #[ignore]
    pub async fn connect_to_kubernetes_api_service_over_ipv6() {
        let app = Application::CurlToKubeApi;
        let mut process = app
            .run_targetless(None, None, Some(vec![("MIRRORD_ENABLE_IPV6", "true")]))
            .await;
        let res = process.wait().await;
        assert!(res.success());
        let stdout = process.get_stdout().await;
        assert!(stdout.contains(r#""apiVersion": "v1""#))
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_traffic_single_request_disabled(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
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
    pub async fn outgoing_traffic_make_request_after_listen(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_make_request_after_listen.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
    }

    /// Verifies that a local socket that has a port subscription can receive traffic
    /// from within the application itself.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn outgoing_connection_to_self(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = ["node", "node-e2e/outgoing/outgoing_connection_to_self.mjs"]
            .map(String::from)
            .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;
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
        #[future] basic_service: KubeService,
        #[future] kube_client: Client,
    ) {
        let internal_service = udp_logger_service.await; // Only reachable from withing the cluster.
        let target_service = basic_service.await; // Impersonate a pod of this service, to reach internal.
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

        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client_with_connect.mjs",
            "31415",
            // Reaching service by only service name is only possible from within the cluster.
            &internal_service.name,
        ]
        .map(String::from)
        .to_vec();

        // Meta-test: verify that the application cannot reach the internal service without
        // mirrord forwarding outgoing UDP traffic via the target pod.
        // If this verification fails, the test itself is invalid.
        let mirrord_no_outgoing = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command.clone(),
            &target_service.pod_container_target(),
            Some(&target_service.namespace),
            Some(mirrord_no_outgoing),
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success()); // The test does not fail, because UDP does not report dropped datagrams.
        let logs = pod_api.logs(&internal_service.pod_name, &lp).await;
        assert_eq!(logs.unwrap(), ""); // Assert that the target service did not get the message.

        // Run mirrord with outgoing enabled.
        let mut process = run_exec_with_target(
            node_command,
            &target_service.pod_container_target(),
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
            .log_stream(&internal_service.pod_name, &lp)
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
        #[future] udp_logger_service: KubeService,
        #[future] basic_service: KubeService,
        #[future] kube_client: Client,
    ) {
        let internal_service = udp_logger_service.await; // Only reachable from withing the cluster.
        let target_service = basic_service.await; // Impersonate a pod of this service, to reach internal.
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

        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client_with_connect.mjs",
            "31415",
            // Reaching service by only service name is only possible from within the cluster.
            &internal_service.name,
        ]
        .map(String::from)
        .to_vec();

        // Make connections on port `31415` go through local.
        let mut config_file = tempfile::Builder::new()
            .prefix("outgoing_traffic_filter_udp_with_connect_local")
            .suffix(".json")
            .tempfile()
            .unwrap();
        let config = serde_json::json!({
            "feature": {
                "network": {
                    "outgoing": {
                        "filter": {
                            "local": [":31415"]
                        }
                    }
                }
            }
        });
        serde_json::to_writer(config_file.as_file_mut(), &config).unwrap();

        // Meta-test: verify that the application cannot reach the internal service without
        // mirrord forwarding outgoing UDP traffic via the target pod.
        // If this verification fails, the test itself is invalid.
        let mut process = run_exec_with_target(
            node_command.clone(),
            &target_service.pod_container_target(),
            Some(&target_service.namespace),
            Some(vec!["--config-file", config_file.path().to_str().unwrap()]),
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(!res.success()); // Should fail because local process cannot reach service.
        let logs = pod_api.logs(&internal_service.pod_name, &lp).await;
        assert_eq!(logs.unwrap(), "");

        // Create remote filter file with service name so we can test DNS outgoing filter.
        let mut remote_config_file = tempfile::Builder::new()
            .prefix("outgoing_traffic_filter_udp_with_connect_remote")
            .suffix(".json")
            .tempfile()
            .unwrap();
        let config = serde_json::json!({
            "feature": {
                "network": {
                    "outgoing": {
                        "filter": {
                            "remote": [internal_service.name]
                        }
                    }
                }
            }
        });
        serde_json::to_writer(remote_config_file.as_file_mut(), &config).unwrap();

        // Run mirrord with outgoing enabled.
        let mut process = run_exec_with_target(
            node_command,
            &target_service.pod_container_target(),
            Some(&target_service.namespace),
            Some(vec![
                "--config-file",
                remote_config_file.path().to_str().unwrap(),
            ]),
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());

        // Verify that the UDP message sent by the application reached the internal service.
        lp.follow = true; // Follow log stream.

        let mut log_lines = pod_api
            .log_stream(&internal_service.pod_name, &lp)
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
    pub async fn outgoing_disabled_udp(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        // Binding specific port, because if we bind 0 then we get a  port that is bypassed by
        // mirrord and then the tested crash is not prevented by the fix but by the bypassed port.
        let socket = UdpSocket::bind("127.0.0.1:31415").unwrap();
        let port = socket.local_addr().unwrap().port().to_string();

        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client_with_connect.mjs",
            &port,
        ]
        .map(String::from)
        .to_vec();
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        // Listen for UDP message directly from application.
        #[cfg(target_os = "windows")]
        const BUF_SIZE: usize = 28;
        #[cfg(not(target_os = "windows"))]
        const BUF_SIZE: usize = 27;

        let mut buf = [0; BUF_SIZE];
        let amt = socket
            .recv(&mut buf)
            .expect("Failed to receive UDP message within timeout");
        assert_eq!(amt, BUF_SIZE);

        let expected_str = {
            #[cfg(target_os = "windows")]
            {
                "Can I pass the test please?\n"
            }
            #[cfg(not(target_os = "windows"))]
            {
                "Can I pass the test please?"
            }
        };
        assert_eq!(buf.get(..amt).unwrap_or(&[]), expected_str.as_bytes()); // Sure you can.

        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn go_outgoing_traffic_single_request_enabled(
        #[values(GoVersion::GO_1_23, GoVersion::GO_1_24, GoVersion::GO_1_25)] go_version: GoVersion,
        #[future] basic_service: KubeService,
    ) {
        let command = vec![format!("go-e2e-outgoing/{go_version}.go_test_app")];
        let service = basic_service.await;
        let mut process =
            run_exec_with_target(command, &service.pod_container_target(), None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    pub async fn go_dns_lookup(
        #[values(GoVersion::GO_1_23, GoVersion::GO_1_24, GoVersion::GO_1_25)] go_version: GoVersion,
        #[future] basic_service: KubeService,
    ) {
        let command = vec![format!("go-e2e-dns/{go_version}.go_test_app")];
        let service = basic_service.await;
        let mut process =
            run_exec_with_target(command, &service.pod_container_target(), None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn listen_localhost(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let node_command = ["node", "node-e2e/listen/test_listen_localhost.mjs"]
            .map(String::from)
            .to_vec();
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    pub async fn gethostname_remote_result(#[future] hostname_service: KubeService) {
        let service = hostname_service.await;
        let command = ["python3", "-u", "python-e2e/hostname.py"]
            .map(String::from)
            .to_vec();
        let mut process =
            run_exec_with_target(command, &service.pod_container_target(), None, None, None).await;

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
    #[cfg_attr(target_os = "windows", ignore)]
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
        basic_service: KubeService,
    ) {
        let service = basic_service.await;
        let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/debug/rust-unix-socket-client")
            .to_string_lossy()
            .to_string();
        let executable = vec![app_path];

        // Tell mirrord to connect remotely to the pathname the deployed app is listening on.
        let env = Some(vec![(
            "MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS",
            "/app/unix-socket-server.sock",
        )]);
        let mut process =
            run_exec_with_target(executable, &service.pod_container_target(), None, None, env)
                .await;
        let res = process.wait().await;

        // The test application panics if it does not successfully connect to the socket, send data,
        // and get the same data back. So if it exits with success everything worked.
        assert!(res.success());
    }

    /// Verify that mirrord does not interfere with ignored unix sockets and connecting to a unix
    /// socket that is NOT configured to happen remotely works fine locally (testing the Bypass
    /// case of connections to unix sockets).
    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn outgoing_bypassed_unix_stream_pathname(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/debug/rust-bypassed-unix-socket")
            .to_string_lossy()
            .to_string();
        let executable = vec![app_path];

        let mut process = run_exec_with_target(
            executable,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;
        let res = process.wait().await;

        // The test application panics if it does not successfully connect to the socket, send data,
        // and get the same data back. So if it exits with success everything worked.
        assert!(res.success());
    }

    /// Verifies that the user application can make requests to some publicly available servers.
    ///
    /// Runs with both enabled and disabled outgoing feature.
    ///
    /// # Note on flakiness
    ///
    /// Because the outcome of this test depends on availability of the servers we
    /// can't control, we first check in the test code whether they're actually available.
    ///
    /// We take the first 10 live servers and pass them to the app.
    /// If we don't have at least 10, we fail.
    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[rstest]
    #[case::outgoing_enabled(true)]
    #[case::outgoing_disabled(false)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_many_requests(
        #[future] basic_service: KubeService,
        #[case] outgoing_enabled: bool,
    ) {
        const HOSTS: &[&str] = &[
            "www.rust-lang.org",
            "www.github.com",
            "www.google.com",
            "www.bing.com",
            "www.yahoo.com",
            "www.twitter.com",
            "www.microsoft.com",
            "www.youtube.com",
            "www.live.com",
            "www.msn.com",
            "www.google.com.br",
            "www.yahoo.co.jp",
            "www.amazon.com",
            "www.wikipedia.org",
            "www.facebook.com",
            "www.drive.google.com",
            "www.gmail.com",
            "www.twitch.tv",
            "www.oracle.com",
            "www.uber.com",
        ];

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(5))
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION"),
            ))
            .build()
            .unwrap();

        let live_hosts = stream::iter(HOSTS)
            .map(|host| {
                let client = client.clone();
                async move {
                    let response = client.get(format!("https://{host}")).send().await;
                    (*host, response)
                }
            })
            .buffer_unordered(3)
            .filter_map(|(host, response)| match response {
                Ok(..) => {
                    println!("{host} is available");
                    std::future::ready(Some(host))
                }
                Err(error) => {
                    println!("{host} is not available: {error}");
                    std::future::ready(None)
                }
            })
            .take(10)
            .collect::<Vec<_>>()
            .await;

        if live_hosts.len() < 10 {
            panic!("Failed to gather 10 available hosts for the test app");
        }

        let as_env = live_hosts.join(",");
        println!("Running test app with AVAILABLE_HOSTS={as_env}");

        let service = basic_service.await;
        let node_command = [
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_many_requests.mjs",
        ]
        .map(String::from)
        .to_vec();
        let mirrord_args = if outgoing_enabled.not() {
            vec!["--no-outgoing"]
        } else {
            Default::default()
        };
        let mut process = run_exec_with_target(
            node_command,
            &service.pod_container_target(),
            None,
            Some(mirrord_args),
            Some(vec![("AVAILABLE_HOSTS", &as_env)]),
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_no_error_in_stderr().await;
    }
}
