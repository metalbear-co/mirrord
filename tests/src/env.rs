#[cfg(test)]

mod env {
    use std::time::Duration;

    use rstest::*;

    use crate::utils::{run_exec, service, KubeService};

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
        let mut process = run_exec(
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
        let mut process = run_exec(
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
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_works(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh"];
        let mut process = run_exec(bash_command, &service.target, None, None, None).await;

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
        let mut process = run_exec(
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
        let mut process = run_exec(
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go18_remote_env_vars_works(#[future] service: KubeService) {
        let service = service.await;
        let command = vec!["go-e2e-env/18"];
        let mut process = run_exec(command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go19_remote_env_vars_works(#[future] service: KubeService) {
        let service = service.await;
        let command = vec!["go-e2e-env/19"];
        let mut process = run_exec(command, &service.target, None, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }
}
