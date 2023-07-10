#[cfg(test)]
mod operator {
    use std::{process::Stdio, time::Duration};

    use rstest::*;
    use tempfile::{tempdir, TempDir};
    use tokio::{io::AsyncWriteExt, process::Command};

    use crate::utils::{run_mirrord, TestProcess};

    pub enum OperatorSetup {
        Online,
        Offline,
    }

    impl OperatorSetup {
        pub fn command_args(&self) -> Vec<&str> {
            match self {
                OperatorSetup::Online => vec![
                    "operator",
                    "setup",
                    "--accept-tos",
                    "--license-key",
                    "my-license-is-cool",
                ],
                OperatorSetup::Offline => {
                    vec![
                        "operator",
                        "setup",
                        "--accept-tos",
                        "--license-path",
                        "./test-license.pem",
                    ]
                }
            }
        }
    }

    async fn check_install_result(stdout: String) {
        let temp_dir = tempdir().unwrap();

        let validate = Command::new("kubectl")
            .args(vec!["apply", "--dry-run=client", "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut validate = TestProcess::from_child(validate, temp_dir).await;

        let mut stdin = validate.child.stdin.take().unwrap();
        stdin.write_all(stdout.as_bytes()).await.unwrap();
        drop(stdin);

        let res = validate.child.wait().await.unwrap();
        let stdout = validate.get_stdout().await;

        assert!(res.success());
        assert!(!stdout.is_empty());
    }

    async fn check_install_file_result(file_path: String, temp_dir: TempDir) {
        let validate = Command::new("kubectl")
            .args(vec!["apply", "--dry-run=client", "-f", &file_path])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut validate = TestProcess::from_child(validate, temp_dir).await;

        let res = validate.child.wait().await.unwrap();
        let stdout = validate.get_stdout().await;

        assert!(res.success());
        assert!(!stdout.is_empty());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn operator_setup(
        #[values(OperatorSetup::Online, OperatorSetup::Offline)] setup: OperatorSetup,
    ) {
        let mut process = run_mirrord(setup.command_args(), Default::default()).await;

        let res = process.child.wait().await.unwrap();
        let stdout = process.get_stdout().await;

        assert!(res.success());
        assert!(!stdout.is_empty());

        check_install_result(stdout).await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn operator_online_setup_file(
        #[values(OperatorSetup::Online, OperatorSetup::Offline)] setup: OperatorSetup,
    ) {
        let temp_dir = tempdir().unwrap();
        let setup_file = temp_dir
            .path()
            .join("operator.yaml")
            .to_str()
            .unwrap_or("operator.yaml")
            .to_owned();

        let mut args = setup.command_args();

        args.push("-f");
        args.push(&setup_file);

        let mut process = run_mirrord(args, Default::default()).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());

        check_install_file_result(setup_file, temp_dir).await;
    }
}
