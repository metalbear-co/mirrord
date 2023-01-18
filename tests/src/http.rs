#[cfg(test)]
mod http {

    use std::time::Duration;

    use kube::Client;
    use rstest::*;
    use tokio::time::timeout;

    use crate::utils::{
        get_service_url, kube_client, send_requests, service, Agent, Application, KubeService,
    };

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
            Application::NodeHTTP
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
        send_requests(&url, false, Default::default()).await;
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
}
