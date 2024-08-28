#![cfg(test)]
#![cfg(feature = "operator")]

use regex::Regex;
use rstest::rstest;

use crate::utils::{run_ls, service_for_mirrord_ls, KubeService};

/// Tests for the `mirrord ls` command with operator
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn mirrord_ls(#[future] service_for_mirrord_ls: KubeService) {
    let service = service_for_mirrord_ls.await;
    let mut process = run_ls::<true>(None, None).await;
    let res = process.wait().await;
    assert!(res.success());
    let stdout = process.get_stdout().await;
    let targets: Vec<String> = serde_json::from_str(&stdout).unwrap();
    let re =
        Regex::new(r"^(pod|deployment|stateful_set|cron_job|job)/.+(/container/.+)?$").unwrap();
    targets
        .iter()
        .for_each(|output| assert!(re.is_match(output)));

    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("pod/{}", service.name))));
    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("deployment/{}", service.name))));
    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("stateful_set/{}", service.name))));
    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("cron_job/{}", service.name))));
    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("job/{}", service.name))));
}
