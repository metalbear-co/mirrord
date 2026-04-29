#![allow(dead_code)]

use mirrord_test_utils::TestProcess;

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
            FileOps::Python => ["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"]
                .map(String::from)
                .to_vec(),
            FileOps::Rust => ["../target/debug/rust-e2e-fileops"]
                .map(String::from)
                .to_vec(),
            FileOps::GoDir(go_version) => vec![format!("go-e2e-dir/{go_version}.go_test_app")],
            FileOps::GoStatfs(go_version) => {
                vec![format!("go-e2e-statfs/{go_version}.go_test_app")]
            }
        }
    }

    pub async fn assert(&self, process: TestProcess) {
        if let FileOps::Python = self {
            process.assert_python_fileops_stderr().await
        }
    }
}
