#[cfg(test)]
mod http_tests {

    use std::time::Duration;

    use kube::Client;
    use rstest::*;

    use crate::utils::{
        application::Application, kube_client, kube_service::KubeService,
        port_forwarder::PortForwarder, send_requests, services::service,
    };

    /// ## Warning
    ///
    /// This test is marked with `ignore` due to flakyness!
    #[ignore]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn mirror_http_traffic(
        #[future]
        #[notrace]
        services: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(
            Application::NodeHTTP,
            Application::Go21HTTP,
            Application::Go22HTTP,
            Application::Go23HTTP,
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP
        )]
        application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let portforwarder = PortForwarder::new(
            kube_client.clone(),
            &service.pod_name,
            &service.namespace,
            80,
        )
        .await;
        let url = format!("http://{}", portforwarder.address());
        let process = application
            .run(
                &service.pod_container_target(),
                Some(&service.namespace),
                None,
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

        application.assert(&process).await;
    }
}
