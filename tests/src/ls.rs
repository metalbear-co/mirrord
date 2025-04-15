#![cfg(test)]

use fancy_regex::Regex;
use rstest::rstest;

#[cfg(feature = "operator")]
use crate::utils::services::operator::service_for_mirrord_ls;
#[cfg(not(feature = "operator"))]
use crate::utils::services::service_for_mirrord_ls;
use crate::utils::{kube_client, run_command::run_ls};

/// Test for the `mirrord ls` command.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn mirrord_ls() {
    let namespace = format!("test-namespace-{:x}", rand::random::<u32>());
    // This function is implemented differently when the `operator` feature is enabled and when it
    // is disabled.
    let _setup = service_for_mirrord_ls(
        &namespace,
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "ls-service",
        false,
        kube_client(),
    )
    .await;

    let mut process = run_ls(&namespace).await;
    let res = process.wait().await;
    assert!(res.success(), "mirrord ls command failed");

    let stdout = process.get_stdout().await;
    let targets: Vec<String> =
        serde_json::from_str(&stdout).expect("mirrord ls output should be a valid JSON");

    let types = [
        "pod",
        "deployment",
        #[cfg(feature = "operator")]
        "statefulset",
        #[cfg(feature = "operator")]
        "cronjob",
        #[cfg(feature = "operator")]
        "job",
        #[cfg(feature = "operator")]
        "replicaset",
        #[cfg(feature = "operator")]
        "service",
    ];

    let pattern = format!(r"^({})/.+(/container/.+)?$", types.join("|"));
    let re = Regex::new(&pattern).unwrap();

    targets.iter().for_each(|output| {
        assert!(
            re.is_match(output).expect("checking regex match panicked"),
            "output line does not match the pattern `{pattern}`: {output}"
        )
    });

    for target_type in types {
        assert!(
            targets
                .iter()
                .any(|output| output.starts_with(&format!("{target_type}/ls-service"))),
            "target type {target_type} was not found in the output",
        )
    }
}
