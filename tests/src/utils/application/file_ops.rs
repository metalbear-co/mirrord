#![allow(dead_code)]

use mirrord_test_utils::{TestProcess, python};

use crate::utils::application::GoVersion;

#[derive(Debug)]
pub enum FileOps {
    Python,
    Rust,
    GoDir(GoVersion),
    GoStatfs(GoVersion),
}

impl FileOps {
    pub fn command(&self) -> Vec<String> {
        match self {
            FileOps::Python => vec![
                python(),
                "-B".into(),
                "-m".into(),
                "unittest".into(),
                "-f".into(),
                "apps/python-e2e/ops/main.py".to_owned(),
            ],
            FileOps::Rust => vec!["apps/rust-e2e-fileops/rust-e2e-fileops".to_owned()],
            FileOps::GoDir(go_version) => {
                vec![format!("apps/go-e2e-dir/go-e2e-dir.{go_version}")]
            }
            FileOps::GoStatfs(go_version) => {
                vec![format!("apps/go-e2e-statfs/go-e2e-statfs.{go_version}")]
            }
        }
    }

    pub async fn assert(&self, process: TestProcess) {
        if let FileOps::Python = self {
            process.assert_python_fileops_stderr().await
        }
    }
}
