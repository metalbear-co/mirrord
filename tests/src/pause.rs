#[cfg(test)]

/// The pause tests use predefined resource names (service/deployment), so they can't be run at the
/// same time. We use `serial_test` to run them one after the other.
mod pause {
    use std::time::Duration;

    use futures::{AsyncBufReadExt, StreamExt};
    use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
    use kube::{
        api::{ListParams, LogParams},
        runtime::wait::{await_condition, conditions::is_job_completed},
        Api, Client, ResourceExt,
    };
    use rstest::*;
    use serial_test::serial;

    use crate::utils::{
        get_service_url, http_log_requester_service, http_logger_service, kube_client,
        random_namespace_self_deleting_service, run_exec_with_target, KubeService,
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
    #[serial]
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

        let log_lines = log_stream.lines();

        // skip 2 lines of flask prints.
        let mut log_lines = log_lines.skip(2);

        let command = vec!["pause/send_reqs.sh"];
        let mirrord_pause_arg = vec!["--pause"];

        let hi_from_deployed_app = "hi-from-deployed-app";
        let hi_from_local_app = "hi-from-local-app";
        let hi_again_from_local_app = "hi-again-from-local-app";

        println!("Waiting for first log by deployed app.");
        let first_log = log_lines.next().await.unwrap().unwrap();

        assert_eq!(first_log, hi_from_deployed_app);

        println!("Running local app with mirrord.");
        let mut process = run_exec_with_target(
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
        let mut next_log = log_lines.next().await.unwrap().unwrap();
        while next_log == hi_from_deployed_app {
            next_log = log_lines.next().await.unwrap().unwrap();
        }

        // Verify first log from local app.
        assert_eq!(next_log, hi_from_local_app);

        // Verify that the second log from local app comes right after it - the deployed requester
        // was paused.
        let log_from_local = log_lines.next().await.unwrap().unwrap();
        assert_eq!(log_from_local, hi_again_from_local_app);

        // Verify that the deployed app resumes after the local app is done.
        let log_from_deployed_after_resume = log_lines.next().await.unwrap().unwrap();
        assert_eq!(log_from_deployed_after_resume, hi_from_deployed_app);
    }

    /// Verify that when running mirrord with the pause feature, and the agent exits early due to an
    /// error, that it unpauses the container before exiting.
    ///
    /// Test Plan:
    ///
    /// 1. Run mirrord with pause and agent error.
    /// 2. Wait for the local child process to exit.
    /// 3. Wait for the agent jobs to complete.
    /// 4. Verify the target pod is unpaused.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    #[serial]
    pub async fn unpause_after_error(
        #[future] random_namespace_self_deleting_service: KubeService,
        #[future] kube_client: Client,
    ) {
        // Using a new random namespace so that we can find the right agent pod.
        let service = random_namespace_self_deleting_service.await;
        let kube_client = kube_client.await;
        let jobs: Api<Job> = Api::namespaced(kube_client.clone(), &service.namespace);

        println!("Running local app with mirrord.");
        let mut process = run_exec_with_target(
            // not specifying so grep waits on stdin.
            vec!["grep", "nothing"],
            &service.target,
            Some(&service.namespace),
            None,
            Some(vec![
                ("MIRRORD_PAUSE", "true"),
                ("MIRRORD_AGENT_TEST_ERROR", "true"),
                ("MIRRORD_AGENT_NAMESPACE", &service.namespace),
            ]),
        )
        .await;

        let res = process.child.wait().await.unwrap();
        println!("mirrord done running.");
        // Expecting the local process with mirrord to fail due to the agent disconnecting.
        assert!(!res.success());

        // Verify the agent pod was deleted before we verify that the target pod is unpaused.
        let lp = ListParams::default().labels("app=mirrord");
        let agent_jobs = jobs.clone().list(&lp).await.unwrap();
        for job in agent_jobs.items {
            println!("Found agent job. Verifying its completion before moving on.",);
            await_condition(jobs.clone(), &job.name_any(), is_job_completed())
                .await
                .unwrap()
                .unwrap();
        }
        println!("Verified all agents completed.");

        // Now verify the remote pod is unpaused after being paused and exiting early with an error.
        let url = get_service_url(kube_client.clone(), &service).await;
        assert!(reqwest::get(url).await.unwrap().status().is_success());
    }
}
