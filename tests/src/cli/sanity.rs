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
    use std::{path::Path, sync::LazyLock, time::Duration};

    use serde_json::{json, Value};

    use rstest::rstest;

    use tracing::debug;

    use crate::utils::{config_dir, ManagedTempFile, run_command::run_verify_config};

    static CONFIG_JSON: LazyLock<Value> = LazyLock::new(|| {
        json!({
            "feature": {
                "network": {
                    "incoming": "mirror",
                    "outgoing": true
                },
                "fs": "read",
                "env": true
            }
        })
    });

    /// Tests `verify-config` with tempfile and `--ide` args, which should be:
    ///
    /// ```sh
    /// mirrord verify-config --ide /path/to/config.json
    /// ```
    #[cfg_attr(target_os = "windows", ignore)]
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]
    pub async fn tempfile_ide_verify_config() {
        debug!("Debug statements initiating...");

        let tempfile = ManagedTempFile::new(CONFIG_JSON.clone());

        let mut process = run_verify_config(Some(vec![
            "--ide",
            tempfile.path.to_str().expect("Valid config path!"),
        ]))
        .await;

        assert!(process.wait().await.success());
    }
    
    /// Tests `verify-config` with only `path` as an arg:
    ///
    /// ```sh
    /// mirrord verify-config /path/to/config.json
    /// ```
    #[cfg_attr(target_os = "windows", ignore)]
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_ide_verify_config(config_dir: &Path) {
        let tempfile = ManagedTempFile::new(CONFIG_JSON.clone());
        let mut config_path = config_dir.to_path_buf();
        config_path.push(&tempfile.path);

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
    #[cfg_attr(target_os = "windows", ignore)]
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_path_verify_config(config_dir: &Path) {
        let tempfile = ManagedTempFile::new(CONFIG_JSON.clone());
        let mut config_path = config_dir.to_path_buf();
        config_path.push(&tempfile.path);

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
    #[cfg_attr(target_os = "windows", ignore)]
    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(30))]
    pub async fn no_path_no_ide_verify_config(config_dir: &Path) {
        let tempfile = ManagedTempFile::new(CONFIG_JSON.clone());
        let mut config_path = config_dir.to_path_buf();
        config_path.push(&tempfile.path);

        let mut process = run_verify_config(None).await;

        assert!(!process.wait().await.success());
    }
}
