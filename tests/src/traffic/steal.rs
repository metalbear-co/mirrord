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
        get_service_url, kube_client, run_exec, send_requests, service, udp_logger_service, Agent,
        Application, KubeService, CONTAINER_NAME,
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

        let mut client = application
            .run(
                &service.target,
                Some(&service.namespace),
                agent.flag().clone(),
                Some(vec![("MIRRORD_HTTP_FILTER", "x-filter=yes")]),
            )
            .await;

        client.wait_for_line(Duration::from_secs(40), "daemon subscribed");

        let mut headers = HeaderMap::default();
        headers.insert("x-filter", "yes".parse().unwrap());
        send_requests(&url, false, headers).await;

        tokio::time::timeout(Duration::from_secs(40), client.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&client);
    }

    #[ignore] // TODO: un-ignore.
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_steal_http_filter_matches_for_client_a(
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

        let mut client_a = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags.clone()),
                Some(vec![("MIRRORD_HTTP_FILTER", "client_a")]),
            )
            .await;

        let mut client_b = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags),
                Some(vec![("MIRRORD_HTTP_FILTER", "client_b")]),
            )
            .await;

        client_a.wait_for_line(Duration::from_secs(40), "daemon subscribed");
        client_b.wait_for_line(Duration::from_secs(40), "daemon subscribed");

        let mut headers = HeaderMap::default();
        headers.insert("Mirrord-Header", "client_a".parse().unwrap());
        send_requests(&url, true, headers).await;

        tokio::time::timeout(Duration::from_secs(40), client_a.child.wait())
            .await
            .unwrap()
            .unwrap();

        tokio::time::timeout(Duration::from_secs(40), client_b.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&client_a);
        application.assert(&client_b);
    }
}
