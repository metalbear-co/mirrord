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

    let expected_target_types = [
        "pod",
        "deployment",
        "statefulset",
        "cronjob",
        "job",
        "replicaset",
        "service",
    ];

    let re = Regex::new(&format!(
        r"^({})/.+(/container/.+)?$",
        expected_target_types.join("|")
    ))
    .unwrap();
    targets.iter().for_each(|output| {
        assert!(
            re.is_match(output),
            "output line {output} does not match regex {re}"
        );
    });

    for target_type in expected_target_types {
        assert!(
            targets
                .iter()
                .any(|output| output.starts_with(&format!("{target_type}/{}", service.name))),
            "no {target_type} target was found in the output",
        );
    }
}
