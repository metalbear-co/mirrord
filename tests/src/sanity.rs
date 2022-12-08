#[cfg(test)]

mod tests {
    use std::{net::UdpSocket, time::Duration};

    use bytes::Bytes;
    use futures::{Future, Stream};
    use futures_util::stream::{StreamExt, TryStreamExt};
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::LogParams, Api, Client};
    use rstest::*;
    use tokio::time::timeout;

    use crate::*;

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_mirror_http_traffic(
        #[future]
        #[notrace]
        service: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(
            Application::NodeHTTP,
            Application::Go18HTTP,
            Application::Go19HTTP,
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP
        )]
        application: Application,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut process = application
            .run(
                &service.target,
                Some(&service.namespace),
                agent.flag(),
                None,
            )
            .await;
        process.wait_for_line(Duration::from_secs(120), "daemon subscribed");
        send_requests(&url, false).await;
        process.wait_for_line(Duration::from_secs(10), "GET");
        process.wait_for_line(Duration::from_secs(10), "POST");
        process.wait_for_line(Duration::from_secs(10), "PUT");
        process.wait_for_line(Duration::from_secs(10), "DELETE");
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[ignore] // TODO: create integration test instead.
    #[cfg(target_os = "macos")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_mirror_http_traffic(
        #[future]
        #[notrace]
        service: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(Application::PythonFlaskHTTP, Application::PythonFastApiHTTP)]
        application: Application,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut process = application
            .run(
                &service.target,
                Some(&service.namespace),
                agent.flag(),
                None,
            )
            .await;
        process.wait_for_line(Duration::from_secs(300), "daemon subscribed");
        send_requests(&url, false).await;
        process.wait_for_line(Duration::from_secs(10), "GET");
        process.wait_for_line(Duration::from_secs(10), "POST");
        process.wait_for_line(Duration::from_secs(10), "PUT");
        process.wait_for_line(Duration::from_secs(10), "DELETE");
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_file_ops(
        #[future]
        #[notrace]
        service: KubeService,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
        #[values(FileOps::Python, FileOps::Go18, FileOps::Go19, FileOps::Rust)] ops: FileOps,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let command = ops.command();

        let mut args = vec!["--fs-mode", "write"];

        if let Some(ephemeral_flag) = agent.flag() {
            args.extend(ephemeral_flag);
        }

        let env = vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/**")];
        let mut process = run(
            command,
            &service.target,
            Some(&service.namespace),
            Some(args),
            Some(env),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        ops.assert(process);
    }

    #[cfg(target_os = "macos")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_file_ops(
        #[future]
        #[notrace]
        service: KubeService,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let python_command = vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"];
        let args = vec!["--fs-mode", "read"];
        let env = vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/fs/**")];

        let mut process = run(
            python_command,
            &service.target,
            Some(&service.namespace),
            Some(args),
            Some(env),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr();
    }

    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_file_ops_ro(
        #[future]
        #[notrace]
        service: KubeService,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let python_command = vec![
            "python3",
            "-B",
            "-m",
            "unittest",
            "-f",
            "python-e2e/files_ro.py",
        ];

        let mut process = run(
            python_command,
            &service.target,
            Some(&service.namespace),
            None,
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_remote_env_vars_exclude_works(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
        ];
        let mirrord_args = vec!["-x", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_remote_env_vars_include_works(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
        ];
        let mirrord_args = vec!["-s", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_remote_dns_enabled_works(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_dns/test_remote_dns_enabled_works.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_remote_dns_lookup_google(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_dns/test_remote_dns_lookup_google.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

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
        send_requests(&url, true).await;
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[rstest]
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_works(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh"];
        let mut process = run(bash_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_exclude_works(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh", "exclude"];
        let mirrord_args = vec!["-x", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(
            bash_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_include_works(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh", "include"];
        let mirrord_args = vec!["-s", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(
            bash_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    // Currently fails due to Layer >> AddressConversion in ci for some reason

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_file_exists(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "exists"];
        let mut process = run(bash_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    // currently there is an issue with piping across forks of processes so 'test_bash_file_read'
    // and 'test_bash_file_write' cannot pass

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_file_read(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "read"];
        let mut process = run(bash_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_file_write(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "write"];
        let args = vec!["--rw"];
        let mut process = run(bash_command, &service.target, None, Some(args), None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go18_remote_env_vars_works(#[future] service: KubeService) {
        let service = service.await;
        let command = vec!["go-e2e-env/18"];
        let mut process = run(command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go19_remote_env_vars_works(#[future] service: KubeService) {
        let service = service.await;
        let command = vec!["go-e2e-env/19"];
        let mut process = run(command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    // TODO: change outgoing TCP tests to use the same setup as in the outgoing UDP test so that
    //       they actually verify that the traffic is intercepted and forwarded (and isn't just
    //       directly sent out from the local application).
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[should_panic]
    pub async fn test_outgoing_traffic_single_request_ipv6(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request_ipv6.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_single_request_disabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ];
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_many_requests_enabled(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_many_requests.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
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
        let mut process = run(
            node_command,
            &service.target,
            None,
            Some(mirrord_args),
            None,
        )
        .await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_make_request_after_listen(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_make_request_after_listen.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_make_request_localhost(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_make_request_localhost.mjs",
        ];
        let mut process = run(node_command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    /// Currently, mirrord only intercepts and forwards outgoing udp traffic if the application
    /// binds a non-0 port and calls `connect`. This test runs with mirrord a node app that does
    /// that and verifies that mirrord intercepts and forwards the outgoing udp message.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_outgoing_traffic_udp_with_connect(
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
        let mut process = run(
            node_command.clone(),
            &target_service.target,
            Some(&target_service.namespace),
            Some(mirrord_no_outgoing),
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(!res.success()); // Should fail because local process cannot reach service.
        let stripped_target = internal_service.target.split('/').collect::<Vec<&str>>()[1];
        let logs = pod_api.logs(stripped_target, &lp).await;
        assert_eq!(logs.unwrap(), "");

        // Run mirrord with outgoing enabled.
        let mut process = run(
            node_command,
            &target_service.target,
            Some(&target_service.namespace),
            None,
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();

        // Verify that the UDP message sent by the application reached the internal service.
        lp.follow = true; // Follow log stream.
        let logs = pod_api
            .log_stream(stripped_target, &lp)
            .await
            .unwrap()
            .try_next()
            .await
            .unwrap()
            .unwrap();
        let logs = String::from_utf8_lossy(&logs);
        assert!(logs.contains("Can I pass the test please?")); // Of course you can.
    }

    /// Test that the process does not crash and messages are sent out normally when the
    /// application calls `connect` on a UDP socket with outgoing traffic disabled on mirrord.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn test_outgoing_disabled_udp(#[future] service: KubeService) {
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
        let mut process = run(
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

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    pub async fn test_go(service: impl Future<Output = KubeService>, command: Vec<&str>) {
        let service = service.await;
        let mut process = run(command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go18_outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let command = vec!["go-e2e-outgoing/18"];
        test_go(service, command).await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go19_outgoing_traffic_single_request_enabled(#[future] service: KubeService) {
        let command = vec!["go-e2e-outgoing/19"];
        test_go(service, command).await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go18_dns_lookup(#[future] service: KubeService) {
        let command = vec!["go-e2e-dns/18"];
        test_go(service, command).await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go19_dns_lookup(#[future] service: KubeService) {
        let command = vec!["go-e2e-dns/19"];
        test_go(service, command).await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_listen_localhost(#[future] service: KubeService) {
        let service = service.await;
        let node_command = vec!["node", "node-e2e/listen/test_listen_localhost.mjs"];
        let mut process = run(node_command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    async fn get_next_log<T: Stream<Item = Result<Bytes, kube::Error>> + Unpin>(
        stream: &mut T,
    ) -> String {
        String::from_utf8_lossy(&stream.try_next().await.unwrap().unwrap()).to_string()
    }

    /// http_logger_service is a service that logs strings from the uri of incoming http requests.
    /// http_log_requester is a service that repeatedly sends a string over requests to the logger.
    /// Deploy the services, the requester sends requests to the logger.
    /// Run a requester with a different string with mirrord with --pause.
    /// verify that the stdout of the logger looks like:
    ///
    /// <string-from-deployed-requester>
    /// <string-from-deployed-requester>
    ///              ...
    /// <string-from-deployed-requester>
    /// <string-from-mirrord-requester>
    /// <string-from-mirrord-requester>
    /// <string-from-deployed-requester>
    /// <string-from-deployed-requester>
    ///              ...
    /// <string-from-deployed-requester>
    ///
    /// Which means the deployed requester was indeed paused while the local requester was running
    /// with mirrord, because local requester waits between its two requests enough time for the
    /// deployed requester to send more requests it were not paused.
    ///
    /// To run on mac, first build universal binary: (from repo root) `scripts/build_fat_mac.sh`
    /// then run test with MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord
    /// Because the test runs a bash script with mirrord and that requires the universal binary.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn pause_log_requests(
        #[future] http_logger_service: KubeService,
        #[future] http_log_requester_service: KubeService,
        #[future] kube_client: Client,
    ) {
        let logger_service = http_logger_service.await;
        let requester_service = http_log_requester_service.await; // Impersonate a pod of this service, to reach internal.
        let kube_client = kube_client.await;
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &logger_service.namespace);

        let target_parts = logger_service.target.split('/').collect::<Vec<&str>>();
        let pod_name = target_parts[1];
        let container_name = target_parts[3];
        let lp = LogParams {
            container: Some(container_name.to_string()), // Default to first, we only have 1.
            follow: true,
            limit_bytes: None,
            pretty: false,
            previous: false,
            since_seconds: None,
            tail_lines: None,
            timestamps: false,
        };

        println!("getting log stream.");
        let log_stream = pod_api.log_stream(pod_name, &lp).await.unwrap();

        let command = vec!["pause/send_reqs.sh"];

        let mirrord_pause_arg = vec!["--pause"];

        println!("Waiting for 2 flask lines.");
        let mut log_stream = log_stream.skip(2); // Skip flask prints.

        let hi_from_deployed_app = "hi-from-deployed-app\n";
        let hi_from_local_app = "hi-from-local-app\n";
        let hi_again_from_local_app = "hi-again-from-local-app\n";

        println!("Waiting for first log by deployed app.");
        let first_log = get_next_log(&mut log_stream).await;

        assert_eq!(first_log, hi_from_deployed_app);

        println!("Running local app with mirrord.");
        let mut process = run(
            command.clone(),
            &requester_service.target,
            Some(&requester_service.namespace),
            Some(mirrord_pause_arg),
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        println!("mirrord done running.");
        assert!(res.success());

        println!("Spooling logs forward to get to local app's first log.");
        // Skip all the logs by the deployed app from before we ran local.
        let mut next_log = get_next_log(&mut log_stream).await;
        while next_log == hi_from_deployed_app {
            next_log = get_next_log(&mut log_stream).await;
        }

        // Verify first log from local app.
        assert_eq!(next_log, hi_from_local_app);

        // Verify that the second log from local app comes right after it - the deployed requester
        // was paused.
        let log_from_local = get_next_log(&mut log_stream).await;
        assert_eq!(log_from_local, hi_again_from_local_app);

        // Verify that the deployed app resumes after the local app is done.
        let log_from_deployed_after_resume = get_next_log(&mut log_stream).await;
        assert_eq!(log_from_deployed_after_resume, hi_from_deployed_app);
    }
}
