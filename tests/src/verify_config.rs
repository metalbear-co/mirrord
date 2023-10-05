#[cfg(test)]
mod verify_config {
    use std::time::Duration;

    use rstest::rstest;

    use crate::utils::{config_dir, run_verify_config};

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn full_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("http_filter_path.json");

        let mut process = run_verify_config(Some(vec![
            "--ide",
            config_path.to_str().expect("Valid config path!"),
        ]))
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_ide_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("http_filter_path.json");

        let mut process = run_verify_config(Some(vec![config_path
            .to_str()
            .expect("Valid config path!")]))
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_path_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("http_filter_path.json");

        let mut process = run_verify_config(Some(vec!["--ide"])).await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_path_no_ide_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("http_filter_path.json");

        let mut process = run_verify_config(None).await;

        let res = process.wait().await;
        assert!(res.success());
    }
}
