#[cfg(test)]
/// Test the targetless execution mode, where an independent agent is spawned - not targeting any
/// existing pod/container/deployment.
mod targetless {
    use std::time::Duration;

    use rstest::rstest;

    use crate::utils::Application;

    /// `mirrord exec` a program that connects to the kubernetes api service by its internal name
    /// from within the cluster, without specifying a target. The http request gets an 403 error
    /// because we do not authenticate, but getting this response means we successfully resolved
    /// dns inside the cluster and connected to a ClusterIP (not reachable from outside of the
    /// cluster).
    ///
    /// Running this test on a cluster that does not have any pods, also proves that we don't use
    /// any existing pod and the agent pod is completely independent.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn connect_to_kubernetes_api_service_with_targetless_agent() {
        let app = Application::CurlToKubeApi;
        let mut process = app.run_targetless(None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        let stdout = process.get_stdout().await;
        assert!(stdout.contains(r#""apiVersion": "v1""#))
    }
}
