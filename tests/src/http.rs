#[cfg(test)]
mod http {

    use std::time::Duration;

    use kube::Client;
    use rstest::*;
    use tokio::time::timeout;

    use crate::utils::{
        get_service_url, kube_client, send_requests, service, Agent, Application, KubeService,
    };

    /// ## Warning
    ///
    /// These tests are marked with `ignore` due to flakyness!
    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn mirror_http_traffic(
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
            Application::Go20HTTP,
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
        process
            .wait_for_line(Duration::from_secs(120), "daemon subscribed")
            .await;
        send_requests(&url, false, Default::default()).await;
        process.wait_for_line(Duration::from_secs(10), "GET").await;
        process.wait_for_line(Duration::from_secs(10), "POST").await;
        process.wait_for_line(Duration::from_secs(10), "PUT").await;
        process
            .wait_for_line(Duration::from_secs(10), "DELETE")
            .await;
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process).await;
    }

    #[ignore] // TODO: create integration test instead.
    #[cfg(target_os = "macos")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn mirror_http_traffic(
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
        process
            .wait_for_line(Duration::from_secs(300), "daemon subscribed")
            .await;
        send_requests(&url, false, Default::default()).await;
        process.wait_for_line(Duration::from_secs(10), "GET").await;
        process.wait_for_line(Duration::from_secs(10), "POST").await;
        process.wait_for_line(Duration::from_secs(10), "PUT").await;
        process
            .wait_for_line(Duration::from_secs(10), "DELETE")
            .await;
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process).await;
    }
}
