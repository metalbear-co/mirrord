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
    let re = Regex::new(r"^(pod|deployment|statefulset|cronjob|job)/.+(/container/.+)?$").unwrap();
    targets
        .iter()
        .for_each(|output| assert!(re.is_match(output)));
    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("pod/{}", service.name))));
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn mirrord_ls_targets(
    #[future] service_for_mirrord_ls: KubeService,
    #[values(
        r"^pod/.+(/container/.+)?$",
        r"^deployment/.+(/container/.+)?$",
        r"^stateful_set/.+(/container/.+)?$",
        r"^cron_job/.+(/container/.+)?$",
        r"^job/.+(/container/.+)?$"
    )]
    target_string: &str,
) {
    let service = service_for_mirrord_ls.await;
    let mut process = run_ls::<true>(None, None).await;
    let res = process.wait().await;
    assert!(res.success());
    let stdout = process.get_stdout().await;
    let targets: Vec<String> = serde_json::from_str(&stdout).unwrap();
    let re = Regex::new(target_string).unwrap();

    let has_target = targets.iter().any(|output| re.is_match(output));
    assert!(has_target, "targets {:?}", targets);

    assert!(targets
        .iter()
        .any(|output| output.starts_with(&format!("pod/{}", service.name))));
}
