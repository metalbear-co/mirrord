#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use core::time::Duration;
use std::path::PathBuf;

use rstest::*;

use crate::utils::{
    config_dir,
    sqs_resources::{sqs_test_resources, write_sqs_messages, SqsTestResources},
    Application, TestProcess,
};

async fn expect_lines<const N: usize>(test_process: &TestProcess) -> Vec<String> {
    let lines = test_process
        .await_n_lines::<N>(Duration::from_secs(10))
        .await;
    assert_eq!(
        lines.len(),
        N,
        "User received more messages than it was supposed to."
    );
    lines
}

/// Verify that the test process printed all the expected messages, and none other.
async fn expect_messages<const N: usize>(messages: [&str; N], test_process: &TestProcess) {
    let lines = expect_lines::<N>(test_process).await;
    for message in messages.into_iter() {
        assert!(
            lines.contains(message.as_ref()),
            "User a was supposed to receive first message but did not"
        );
    }
}

/// Verify that the test process printed all the expected messages, and none other.
async fn expect_messages_in_order<const N: usize>(messages: [&str; N], test_process: &TestProcess) {
    let lines = expect_lines::<N>(test_process).await;
    assert_eq!(
        &lines[..],
        &messages[..],
        "User did not receive the expected messages in the expected order."
    )
}

/// This test creates a new sqs_queue with a random name and credentials from env.
///
/// Define a queue splitter for a deployment. Start two services that both consume from an SQS
/// queue, send some messages to the queue, verify that each of the applications running with
/// mirrord get exactly the messages they are supposed to, and that the deployed application gets
/// the rest.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn two_users_one_queue(
    #[future] sqs_test_resources: SqsTestResources,
    config_dir: &PathBuf,
) {
    let sqs_test_resources = sqs_test_resources.await;
    let application = Application::RustSqs;

    let mut config_path = config_dir.clone();
    config_path.push("sqs_queue_splitting_a.json");

    println!("Starting first mirrord client");
    let mut client_a = application
        .run(
            &sqs_test_resources.deployment_target(),
            Some(sqs_test_resources.namespace()),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    let mut config_path = config_dir.clone();
    config_path.push("sqs_queue_splitting_b.json");

    println!("Starting second mirrord client");
    let mut client_b = application
        .run(
            &sqs_test_resources.deployment_target(),
            Some(sqs_test_resources.namespace()),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    write_sqs_messages(
        sqs_test_resources.sqs_client(),
        sqs_test_resources.queue1(),
        "local",
        &["a", "b", "c", "c", "b", "a"],
        &["1", "2", "3", "4", "5", "6"],
    )
    .await;

    expect_messages(["1", "6"], &client_a).await;
    expect_messages(["2", "5"], &client_b).await;

    write_sqs_messages(
        sqs_test_resources.sqs_client(),
        sqs_test_resources.queue2(),
        "local",
        &["a", "b", "c", "c", "b", "a"],
        &["10", "20", "30", "40", "50", "60"],
    )
    .await;

    expect_messages_in_order(["10", "60"], &client_a).await;
    expect_messages_in_order(["20", "50"], &client_b).await;

    client_a.child.kill().await.unwrap();
    client_b.child.kill().await.unwrap();
}
