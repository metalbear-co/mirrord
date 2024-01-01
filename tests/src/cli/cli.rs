/// # Attention
///
/// If any of these tests fails, then you're attempting a breaking change!
///
/// Do **NOT** modify these tests if you change the cli for `verify-config`, unless we're ok with
/// a breaking change!
///
/// You should probably only add new tests here.
#[cfg(test)]
mod cli {
    use std::time::Duration;

    use regex::Regex;
    use rstest::rstest;

    use crate::utils::{config_dir, run_ls, run_verify_config, service, KubeService};

    /// Tests `verify-config` with `path` and `--ide` args, which should be:
    ///
    /// ```sh
    /// mirrord verify-config --ide /path/to/config.json
    /// ```
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]    
    pub async fn path_ide_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("default_ide.json");

        let mut process = run_verify_config(Some(vec![
            "--ide",
            config_path.to_str().expect("Valid config path!"),
        ]))
        .await;

        assert!(process.wait().await.success());
    }

    /// Tests `verify-config` with only `path` as an arg:
    ///
    /// ```sh
    /// mirrord verify-config /path/to/config.json
    /// ```
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_ide_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("default_ide.json");

        let mut process = run_verify_config(Some(vec![config_path
            .to_str()
            .expect("Valid config path!")]))
        .await;

        assert!(process.wait().await.success());
    }

    /// Tests `verify-config` with only `--ide` as an arg:
    ///
    /// The process should fail, as path is a required arg!
    ///
    /// ```sh
    /// mirrord verify-config --ide
    /// ```
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]    
    pub async fn no_path_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("default_ide.json");

        let mut process = run_verify_config(Some(vec!["--ide"])).await;

        assert!(!process.wait().await.success());
    }

    /// Tests `verify-config` without args:
    ///
    /// The process should fail, as path is a required arg!
    ///
    /// ```sh
    /// mirrord verify-config
    /// ```
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]    
    pub async fn no_path_no_ide_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("default_ide.json");

        let mut process = run_verify_config(None).await;

        assert!(!process.wait().await.success());
    }

    /// Tests for the `mirrord ls` command
    #[rstest]    
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn mirrord_ls(#[future] service: KubeService) {
        let service = service.await;
        let mut process = run_ls(None, None).await;
        let res = process.wait().await;
        assert!(res.success());
        let stdout = process.get_stdout().await;
        let targets: Vec<String> = serde_json::from_str(&stdout).unwrap();
        let re = Regex::new(r"^(pod|deployment)/.+(/container/.+)?$").unwrap();
        targets
            .iter()
            .for_each(|output| assert!(re.is_match(output)));
        assert!(targets
            .iter()
            .any(|output| output.starts_with(&format!("pod/{}", service.name))));
    }
}
