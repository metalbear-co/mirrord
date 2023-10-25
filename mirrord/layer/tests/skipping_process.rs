#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{env::temp_dir, os::unix::fs, path::PathBuf, time::Duration};

use rand::prelude::*;
use rstest::rstest;

mod common;
pub use common::*;

/// Creates a new [`Application`], which is a symlink to the existing executable.
/// The link is created in the temporary directory and its name is randomized.
async fn symlink_app(app: &Application) -> Application {
    let path = temp_dir().join(format!(
        "test-dynamic-app-{}",
        rand::thread_rng().gen::<u128>()
    ));
    println!("{}", app.get_executable().await);

    fs::symlink(app.get_executable().await, &path).expect("failed to create executable symlink");

    Application::DynamicApp(path.to_str().unwrap().to_string(), vec![])
}

// This doesn't work on macOS, probably different way it determines executable.
#[cfg(not(target_os = "macos"))]
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn skip_based_on_exec_name(dylib_path: &PathBuf) {
    let app = Application::OpenFile;
    let symlinked_app = symlink_app(&app).await;

    let ignore = app
        .get_executable()
        .await
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    let (mut test_process, _intproxy) = symlinked_app
        .start_process_with_layer(dylib_path, vec![("MIRRORD_SKIP_PROCESSES", &ignore)], None)
        .await;

    test_process.wait_assert_success().await;
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn skip_based_on_invocation_name(dylib_path: &PathBuf) {
    let app = Application::OpenFile;
    let symlinked_app = symlink_app(&app).await;

    let ignore = symlinked_app
        .get_executable()
        .await
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    let (mut test_process, _intproxy) = symlinked_app
        .start_process_with_layer(dylib_path, vec![("MIRRORD_SKIP_PROCESSES", &ignore)], None)
        .await;

    test_process.wait_assert_success().await;
}

/// This is just a sanity test. Tests whether the application actually uses the layer.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn dont_skip(dylib_path: &PathBuf) {
    let app = Application::OpenFile;
    let symlinked_app = symlink_app(&app).await;

    let (mut test_process, mut intproxy) = symlinked_app
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    intproxy
        .expect_file_open_for_reading("/etc/resolv.conf", 5)
        .await;

    test_process.wait_assert_success().await;
}
