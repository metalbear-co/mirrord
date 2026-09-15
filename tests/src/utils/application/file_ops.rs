#![allow(dead_code)]

use mirrord_test_utils::TestProcess;

use crate::utils::application::{app_path, GoVersion};

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
                "python3".into(),
                "-B".into(),
                "-m".into(),
                "unittest".into(),
                "-f".into(),
                app_path("python-e2e/ops.py"),
            ],
            FileOps::Rust => vec![app_path("../target/debug/rust-e2e-fileops")],
            FileOps::GoDir(go_version) => {
                vec![app_path(&format!("go-e2e-dir/{go_version}.go_test_app"))]
            }
            FileOps::GoStatfs(go_version) => {
                vec![app_path(&format!("go-e2e-statfs/{go_version}.go_test_app"))]
            }
        }
    }

    pub async fn assert(&self, process: TestProcess) {
        if let FileOps::Python = self {
            process.assert_python_fileops_stderr().await
        }
    }
}
