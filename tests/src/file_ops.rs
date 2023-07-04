#[cfg(test)]
mod file_ops {

    use std::time::Duration;

    use rstest::*;

    use crate::utils::{run_exec_with_target, service, Agent, FileOps, KubeService};

    #[cfg(target_os = "linux")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn file_ops(
        #[future]
        #[notrace]
        service: KubeService,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
        #[values(FileOps::Python, FileOps::Rust)] ops: FileOps,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let command = ops.command();

        let mut args = vec!["--fs-mode", "write"];

        if let Some(ephemeral_flag) = agent.flag() {
            args.extend(ephemeral_flag);
        }

        let env = vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/**")];
        let mut process = run_exec_with_target(
            command,
            &service.target,
            Some(&service.namespace),
            Some(args),
            Some(env),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        ops.assert(process).await;
    }

    #[cfg(target_os = "macos")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn file_ops(
        #[future]
        #[notrace]
        service: KubeService,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let python_command = vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"];
        let args = vec!["--fs-mode", "read"];
        let env = vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp**")];

        let mut process = run_exec_with_target(
            python_command,
            &service.target,
            Some(&service.namespace),
            Some(args),
            Some(env),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr().await;
    }

    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn file_ops_ro(
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

        let mut process = run_exec_with_target(
            python_command,
            &service.target,
            Some(&service.namespace),
            None,
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr().await;
    }

    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn statfs(
        #[future]
        #[notrace]
        service: KubeService,
    ) {
        let service = service.await;
        let python_command = vec![
            "python3",
            "-B",
            "-m",
            "unittest",
            "-f",
            "python-e2e/statfs.py",
        ];

        let mut process = run_exec_with_target(
            python_command,
            &service.target,
            Some(&service.namespace),
            None,
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr().await;
    }

    // Currently fails due to Layer >> AddressConversion in ci for some reason

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_file_exists(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "exists"];
        let mut process =
            run_exec_with_target(bash_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
    }

    // currently there is an issue with piping across forks of processes so 'test_bash_file_read'
    // and 'test_bash_file_write' cannot pass

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_file_read(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "read"];
        let mut process =
            run_exec_with_target(bash_command, &service.target, None, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
    }

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_file_write(#[future] service: KubeService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "write"];
        let args = vec!["--rw"];
        let mut process =
            run_exec_with_target(bash_command, &service.target, None, Some(args), None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
    }

    /// On Linux: Test our getdents64 Go syscall hook, for `os.ReadDir` on go.
    /// This is an E2E test and not an integration test in order to test the agent side of the
    /// detour.
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn go_dir(
        #[future]
        #[notrace]
        service: KubeService,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
        #[values(FileOps::GoDir18, FileOps::GoDir19, FileOps::GoDir20)] ops: FileOps,
    ) {
        let service = service.await;
        let command = ops.command();

        let mut args = vec!["--fs-mode", "read"];

        if let Some(ephemeral_flag) = agent.flag() {
            args.extend(ephemeral_flag);
        }

        let mut process = run_exec_with_target(
            command,
            &service.target,
            Some(&service.namespace),
            Some(args),
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
    }
}
