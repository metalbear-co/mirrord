#![allow(dead_code)]

use crate::utils::TestProcess;

#[derive(Debug)]
pub enum FileOps {
    Python,
    Rust,
    GoDir21,
    GoDir22,
    GoDir23,
    GoStatfs,
}

impl FileOps {
    pub fn command(&self) -> Vec<&str> {
        match self {
            FileOps::Python => {
                vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"]
            }
            FileOps::Rust => vec!["../target/debug/rust-e2e-fileops"],
            FileOps::GoDir21 => vec!["go-e2e-dir/21.go_test_app"],
            FileOps::GoDir22 => vec!["go-e2e-dir/22.go_test_app"],
            FileOps::GoDir23 => vec!["go-e2e-dir/23.go_test_app"],
            FileOps::GoStatfs => vec!["go-e2e-statfs/23.go_test_app"],
        }
    }

    pub async fn assert(&self, process: TestProcess) {
        if let FileOps::Python = self {
            process.assert_python_fileops_stderr().await
        }
    }
}
