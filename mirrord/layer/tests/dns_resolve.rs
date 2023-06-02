#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[timeout(Duration::from_secs(60))]
async fn test_dns_resolve(
    #[values(Application::NodeDnsResolve, Application::NodeRawDnsResolve)] application: Application,
    dylib_path: &PathBuf,
) {
}
