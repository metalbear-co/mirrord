#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use core::time::Duration;
use std::{ops::Not, path::PathBuf};

use rstest::*;

use crate::utils::{
    config_dir,
    sqs_resources::{sqs_test_resources, write_sqs_messages, SqsTestResources},
    Application, TestProcess,
};

async fn expect_lines<const N: usize>(test_process: &TestProcess) -> Vec<String> {
    let lines = test_process
        .await_n_lines::<N>(Duration::from_secs(30))
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
            lines.contains(&message.to_string()),
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

/// Verify that the echo queue contains the expected messages, meaning the deployed application
/// received all the messages no user filtered.
async fn expect_messages_in_queue<const N: usize>(
    messages: [&str; N],
    client: &aws_sdk_sqs::Client,
    q_name: &str,
) {
    // TODO!
}

/// Verify that the echo queue contains the expected messages, meaning the deployed application
/// received all the messages no user filtered.
/// Also verify the message order was preserved.
async fn expect_messages_in_fifo_queue<const N: usize>(
    messages: [&str; N],
    client: &aws_sdk_sqs::Client,
    q_name: &str,
) {
    // TODO!
}

async fn verify_splitter_temp_queues_deleted(sqs_test_resources: &SqsTestResources) {
    let client = sqs_test_resources.sqs_client();
    let queue_name1 = sqs_test_resources.queue1().name.as_str();
    let queue_name2 = sqs_test_resources.queue2().name.as_str();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if client
                .list_queues()
                // temp queues start with "mirrord-"
                .queue_name_prefix("mirrord-")
                .send()
                .await
                .expect("Could not list SQS queues.")
                .queue_urls
                .unwrap_or_default()
                .iter()
                // temp queue names contain the original queue name.
                .any(|queue_url| queue_url.contains(queue_name1) || queue_url.contains(queue_name2))
                .not()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("SQS temp queues not deleted in 60 seconds.")
}

/// This test creates a new sqs_queue with a random name and credentials from env.
///
/// Define a queue splitter for a deployment. Start two services that both consume from an SQS
/// queue, send some messages to the queue, verify that each of the applications running with
/// mirrord get exactly the messages they are supposed to, and that the deployed application gets
/// the rest.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn two_users(#[future] sqs_test_resources: SqsTestResources, config_dir: &PathBuf) {
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
    // TODO: implement func
    expect_messages_in_queue(
        ["3", "4"],
        sqs_test_resources.sqs_client(),
        sqs_test_resources.queue1().name.as_str(),
    )
    .await;

    println!("Queue 1 was split correctly!");

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
    // TODO: implement func
    expect_messages_in_fifo_queue(
        ["30", "40"],
        sqs_test_resources.sqs_client(),
        sqs_test_resources.queue2().name.as_str(),
    )
    .await;

    println!("Queue 2 was split correctly!");

    // TODO: verify queue tags.

    client_a.child.kill().await.unwrap();
    client_b.child.kill().await.unwrap();

    verify_splitter_temp_queues_deleted(&sqs_test_resources).await;
    println!("All temporary queues were deleted!");
}
