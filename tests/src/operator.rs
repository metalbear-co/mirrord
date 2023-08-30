#[cfg(test)]
mod operator {
    use std::{process::Stdio, time::Duration};

    use k8s_openapi::api::apps::v1::Deployment;
    use kube::Client;
    use rstest::*;
    use tempfile::{tempdir, TempDir};
    use tokio::{io::AsyncWriteExt, process::Command};

    use crate::utils::{
        get_instance_name, kube_client, run_mirrord, service, Application, KubeService, TestProcess,
    };

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
        let mut validate = TestProcess::from_child(validate, Some(temp_dir)).await;

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
        let mut validate = TestProcess::from_child(validate, Some(temp_dir)).await;

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

        let res = process.wait().await;
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

        let res = process.wait().await;
        assert!(res.success());

        check_install_file_result(setup_file, temp_dir).await;
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn two_clients_steal_same_target(
        #[future]
        #[notrace]
        service: KubeService,
        #[values(Application::PythonFlaskHTTP)] application: Application,
    ) {
        if let Ok(_) = std::env::var("MIRRORD_OPERATOR_TESTS") {
            let service = service.await;

            let flags = vec!["--steal"];

            let mut client_a = application
                .run(
                    &service.target,
                    Some(&service.namespace),
                    Some(flags.clone()),
                    None,
                )
                .await;

            client_a
                .wait_for_line(Duration::from_secs(40), "daemon subscribed")
                .await;

            let mut client_b = application
                .run(&service.target, Some(&service.namespace), Some(flags), None)
                .await;

            client_b
                .wait_for_line(Duration::from_secs(40), "Someone else is stealing traffic")
                .await;

            client_a.child.kill().await.unwrap();

            let res = client_b.child.wait().await.unwrap();
            assert!(!res.success());
        }
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn two_clients_steal_same_target_pod_deployment(
        #[future]
        #[notrace]
        service: KubeService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(Application::PythonFlaskHTTP)] application: Application,
    ) {
        // if let Ok(_) = std::env::var("MIRRORD_OPERATOR_TESTS") {
        let service = service.await;

        let flags = vec!["--steal"];

        let mut client_a = application
            .run(
                &service.target,
                Some(&service.namespace),
                Some(flags.clone()),
                None,
            )
            .await;

        client_a
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;

        let target =
            get_instance_name::<Deployment>(kube_client.await, &service.name, &service.namespace)
                .await
                .unwrap();

        let mut client_b = application
            .run(
                &format!("deployment/{target}"),
                Some(&service.namespace),
                Some(flags.clone()),
                None,
            )
            .await;

        client_b
            .wait_for_line(Duration::from_secs(40), "Someone else is stealing traffic")
            .await;

        client_a.child.kill().await.unwrap();

        let res = client_b.child.wait().await.unwrap();
        assert!(!res.success());
    }
    // }
}
