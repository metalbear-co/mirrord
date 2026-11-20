#![cfg(test)]

use fancy_regex::Regex;
use mirrord_test_utils::run_command::run_ls;
use rstest::rstest;

use crate::utils::{client::kube_client, images::PYTEST_IMAGE, services::basic_service};

/// Test for the `mirrord ls` command.
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test]
pub async fn mirrord_ls() {
    let namespace = format!("test-namespace-{:x}", rand::random::<u32>());
    let _setup = basic_service(
        &namespace,
        "NodePort",
        PYTEST_IMAGE,
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

    let types = ["pod", "deployment"];

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
