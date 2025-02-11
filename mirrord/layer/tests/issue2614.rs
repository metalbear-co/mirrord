#![cfg(target_os = "linux")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

use rand::Rng;
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2614](https://github.com/metalbear-co/mirrord/issues/2614) is fixed
/// and the file open mode is honoured on bypass.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2614(dylib_path: &Path) {
    let tmpdir = tempfile::tempdir().unwrap();
    let file_path = tmpdir
        .path()
        .join(format!("testfile-{}", rand::random::<u64>(),));
    let application = Application::Go23Open {
        path: file_path.to_str().unwrap().into(),
        flags: libc::O_CREAT | libc::O_RDWR,
        mode: libc::S_IRUSR | libc::S_IRGRP | libc::S_IROTH,
    };
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_FILE_MODE", "local")], None)
        .await;

    let message = intproxy.try_recv().await;
    assert!(
        message.is_none(),
        "received an unexpected message: {message:?}"
    );
    test_process.wait_assert_success().await;

    let permissions = tokio::fs::metadata(file_path).await.unwrap().permissions();
    assert_eq!(
        permissions.mode() & 0b111111111,
        libc::S_IRUSR | libc::S_IRGRP | libc::S_IROTH,
        "test app created file with unexpected permissions"
    )
}
