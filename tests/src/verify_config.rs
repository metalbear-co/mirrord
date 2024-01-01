/// # Attention
///
/// If any of these tests fails, then you're attempting a breaking change!
///
/// Do **NOT** modify these tests if you change the cli for `verify-config`, unless we're ok with
/// a breaking change!
///
/// You should probably only add new tests here.
#[cfg(test)]
mod verify_config {
    use std::time::Duration;

    use rstest::rstest;

    use crate::utils::{config_dir, run_verify_config};

    /// Tests `verify-config` with `path` and `--ide` args, which should be:
    ///
    /// ```sh
    /// mirrord verify-config --ide /path/to/config.json
    /// ```
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]
    #[cfg_attr(not(feature = "mirrord-cli"), ignore)]
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
    #[cfg_attr(not(feature = "mirrord-cli"), ignore)]
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
    #[cfg_attr(not(feature = "mirrord-cli"), ignore)]
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
    #[cfg_attr(not(feature = "mirrord-cli"), ignore)]
    pub async fn no_path_no_ide_verify_config(config_dir: &std::path::PathBuf) {
        let mut config_path = config_dir.clone();
        config_path.push("default_ide.json");

        let mut process = run_verify_config(None).await;

        assert!(!process.wait().await.success());
    }
}
