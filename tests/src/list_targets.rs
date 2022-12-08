#[cfg(test)]
/// Tests for the `mirrord ls` command
mod list_targets {
    use std::process::Stdio;

    use regex::Regex;
    use rstest::rstest;
    use tokio::process::Command;

    use crate::tests::{service, KubeService, TestProcess};

    /// Runs `mirrord ls` command and asserts if the json matches the expected format
    async fn run_ls(args: Option<Vec<&str>>, namespace: Option<&str>) -> TestProcess {
        let path = match option_env!("MIRRORD_TESTS_USE_BINARY") {
            None => env!("CARGO_BIN_FILE_MIRRORD"),
            Some(binary_path) => binary_path,
        };
        let temp_dir = tempdir::TempDir::new("test").unwrap();
        let mut mirrord_args = vec!["ls"];
        if let Some(args) = args {
            mirrord_args.extend(args);
        }
        if let Some(namespace) = namespace {
            mirrord_args.extend(vec!["--namespace", namespace]);
        }

        let process = Command::new(path)
            .args(mirrord_args.clone())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        println!(
            "executed mirrord with args {mirrord_args:?} pid {}",
            process.id().unwrap()
        );
        TestProcess::from_child(process, temp_dir)
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_mirrord_ls(#[future] service: KubeService) {
        service.await;
        let mut process = run_ls(None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        let stdout = process.get_stdout();
        let targets: Vec<String> = serde_json::from_str(&stdout).unwrap();
        let re = Regex::new(r"^pod/.+(/container/.+)?$").unwrap();
        targets
            .iter()
            .for_each(|output| assert!(re.is_match(output)));
        process.assert_stderr();
    }
}
