#![cfg(test)]
#![cfg(feature = "operator")]
use std::time::Duration;

use regex::Regex;
use rstest::rstest;

use crate::utils::{config_dir, run_ls, run_verify_config, service, KubeService};

/// Tests for the `mirrord ls` command with operator
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn mirrord_ls(#[future] service: KubeService) {
    let service = service.await;
    let mut process = run_ls::<true>(None, None).await;
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
