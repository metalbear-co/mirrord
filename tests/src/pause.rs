#[cfg(test)]

mod pause {

    use std::time::Duration;

    use futures::StreamExt;
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::LogParams, Api, Client};
    use rstest::*;

    use crate::utils::{
        get_next_log, http_log_requester_service, http_logger_service, kube_client, run_exec,
        KubeService,
    };

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
        println!("{:#?}: starting test", std::time::SystemTime::now());
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

        println!(
            "{:#?}: Running local app with mirrord.",
            std::time::SystemTime::now()
        );
        let mut process = run_exec(
            command.clone(),
            &requester_service.target,
            Some(&requester_service.namespace),
            Some(mirrord_pause_arg),
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        println!("{:#?}: mirrord done running.", std::time::SystemTime::now());
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
