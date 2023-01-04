#[cfg(test)]
mod steal {
    use std::{net::UdpSocket, time::Duration};

    use futures::Future;
    use futures_util::stream::TryStreamExt;
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::LogParams, Api, Client};
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    use rstest::*;

    use crate::utils::{
        get_service_url, kube_client, run_exec, send_request, send_requests, service,
        udp_logger_service, Agent, Application, KubeService, CONTAINER_NAME,
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
    #[timeout(Duration::from_secs(240))]
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
                Some(vec![("MIRRORD_HTTP_FILTER", "x-filter=yes")]),
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
    #[timeout(Duration::from_secs(240))]
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
                Some(vec![("MIRRORD_HTTP_FILTER", "x-filter=yes")]),
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
}
