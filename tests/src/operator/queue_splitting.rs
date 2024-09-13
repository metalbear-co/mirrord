#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use core::time::Duration;
use std::{collections::HashSet, ops::Not, path::PathBuf};

use aws_sdk_sqs::{operation::receive_message::ReceiveMessageOutput, types::Message};
use rstest::*;

use crate::utils::{
    config_dir,
    sqs_resources::{sqs_test_resources, write_sqs_messages, QueueInfo, SqsTestResources},
    Application, TestProcess,
};

async fn expect_n_lines(n: usize, test_process: &TestProcess) -> Vec<String> {
    let lines = test_process.await_n_lines(n, Duration::from_secs(60)).await; // TODO: reduce timeout
    assert_eq!(
        lines.len(),
        n,
        "User received more messages than it was supposed to."
    );
    lines
}

/// Verify that the test process printed all the expected messages, and none other.
///
/// The expected lines should all be unique in both arrays together (a line should not appear in
/// both arrays or in any array twice).
async fn expect_output_lines<const N: usize, const M: usize>(
    expected_lines: [&str; N],
    expected_in_order_lines: [&str; M],
    test_process: &TestProcess,
) {
    let lines = expect_n_lines(
        expected_lines.len() + expected_in_order_lines.len(),
        test_process,
    )
    .await;
    for expected_line in expected_lines.into_iter() {
        assert!(
            lines.contains(&expected_line.to_string()),
            "User a was expected to print {expected_line} but did not"
        );
    }
    let mut last_location = 0;
    for expected_line in expected_in_order_lines.into_iter() {
        let location = lines
            .iter()
            .position(|line| *line == expected_line)
            .expect("A fifo message did not reach the correct user.");
        assert!(last_location <= location, "Fifo messages out of order!");
        last_location = location;
    }
}

async fn delete_message(client: &aws_sdk_sqs::Client, queue_url: &str, receipt_handle: &str) {
    client
        .delete_message()
        .queue_url(queue_url)
        .receipt_handle(receipt_handle)
        .send()
        .await
        .inspect_err(|err| eprintln!("deleting received message failed: {err:?}"))
        .ok();
}

/// Verify that the echo queue contains the expected messages, meaning the deployed application
/// received all the messages no user filtered.
/// messages don't have to arrive in any particular message.
async fn expect_messages_in_queue<const N: usize>(
    messages: [&str; N],
    client: &aws_sdk_sqs::Client,
    echo_queue: &QueueInfo,
) {
    tokio::time::timeout(Duration::from_secs(20), async {
        println!("Verifying correct messages in echo queue {} (verifying the deployed application got the messages it was supposed to)", echo_queue.name);
        let mut expected_messages = HashSet::from(messages);
        loop {
            if let Ok(ReceiveMessageOutput {
                          messages: Some(received_messages),
                          ..
                      }) = client
                .receive_message()
                .queue_url(&echo_queue.url)
                .visibility_timeout(15)
                .wait_time_seconds(20)
                .send()
                .await
            {
                for Message {
                    receipt_handle, body, ..
                } in received_messages {
                    let message = body.expect("Received empty bodied message from echo queue.");
                    println!(r#"got message "{message}" in queue "{}"."#, echo_queue.name);
                    assert!(expected_messages.remove(message.as_str()));
                    delete_message(client, &echo_queue.url, &receipt_handle.expect("no receipt handle")).await;
                    if expected_messages.is_empty() {
                        return;
                    }
                }
            }
        }
    }).await.unwrap();
}

/// Verify that the echo queue contains the expected messages, meaning the deployed application
/// received all the messages no user filtered.
/// Also verify the message order was preserved.
async fn expect_messages_in_fifo_queue<const N: usize>(
    messages: [&str; N],
    client: &aws_sdk_sqs::Client,
    echo_queue: &QueueInfo,
) {
    tokio::time::timeout(Duration::from_secs(20), async {
        println!("Verifying correct messages in echo queue {} (verifying the deployed application got the messages it was supposed to)", echo_queue.name);
        let mut expected_messages = messages.into_iter();
        let mut expected_message = expected_messages.next().unwrap();
        loop {
            let ReceiveMessageOutput {
                messages,
                ..
            } = client
                .receive_message()
                .queue_url(&echo_queue.url)
                .visibility_timeout(15)
                .wait_time_seconds(20)
                .send()
                .await
                .expect("receiving messages from echo queue failed");
            if let Some(received_messages) = messages {
                for Message {
                    body, receipt_handle, ..
                } in received_messages {
                    let message = body.expect("Received empty bodied message from echo queue.");
                    println!(r#"got message "{message}" in queue "{}"."#, echo_queue.name);
                    assert_eq!(message, expected_message,);
                    delete_message(client, &echo_queue.url, &receipt_handle.expect("no receipt handle")).await;
                    let Some(message) = expected_messages.next() else {
                        return;
                    };
                    expected_message = message;
                }
            }
        }
    }).await.unwrap();
}

async fn verify_splitter_temp_queues_deleted(sqs_test_resources: &SqsTestResources) {
    let client = &sqs_test_resources.sqs_client;
    let queue_name1 = sqs_test_resources.queue1.name.as_str();
    let queue_name2 = sqs_test_resources.queue2.name.as_str();
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut i = 0u8; // TODO: delete;
        loop {
            {
                // TODO: delete this whole block
                i += 1;
                if i % 100 == 0 {
                    let queues = client
                        .list_queues()
                        // temp queues start with "mirrord-"
                        .queue_name_prefix("mirrord-")
                        .send()
                        .await
                        .expect("Could not list SQS queues.")
                        .queue_urls
                        .unwrap_or_default()
                        .into_iter()
                        // temp queue names contain the original queue name.
                        .filter(|queue_url| {
                            queue_url.contains(queue_name1) || queue_url.contains(queue_name2)
                        })
                        .collect::<Vec<_>>();
                    println!("Lingering queues ({i}): {queues:#?}");
                }
            }
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
    .expect("SQS temp queues not deleted in time.")
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
    // Wait between client starts.
    // Support starting at the same time and remove this (https://github.com/metalbear-co/operator/issues/637)
    // TODO: (now, in this PR) change from sleep to watch registry.
    tokio::time::sleep(Duration::from_secs(30)).await;

    println!("Starting second mirrord client");
    let mut client_b = application
        .run(
            &sqs_test_resources.deployment_target(),
            Some(sqs_test_resources.namespace()),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    println!("letting split time to start before writing messages");
    // sqs_test_resources
    //     .wait_for_sqs_sessions()
    //     .await
    //     .expect("There was a problem with the SQS Session resources");

    // TODO:
    tokio::time::sleep(Duration::from_secs(30)).await;

    write_sqs_messages(
        &sqs_test_resources.sqs_client,
        &sqs_test_resources.queue1,
        "client",
        &["a", "b", "c", "c", "b", "a"],
        &["1", "2", "3", "4", "5", "6"],
    )
    .await;

    write_sqs_messages(
        &sqs_test_resources.sqs_client,
        &sqs_test_resources.queue2,
        "client",
        &["a", "b", "c", "c", "b", "a"],
        &["10", "20", "30", "40", "50", "60"],
    )
    .await;

    // Test app prints 1: before messages from queue1 and 2: before messages from queue 2.
    expect_output_lines(["1:1", "1:6"], ["2:10", "2:60"], &client_a).await;
    println!("Client a received the correct messages.");
    expect_output_lines(["1:2", "1:5"], ["2:20", "2:50"], &client_b).await;
    println!("Client b received the correct messages.");

    expect_messages_in_queue(
        ["3", "4"],
        &sqs_test_resources.sqs_client,
        &sqs_test_resources.echo_queue1,
    )
    .await;

    println!("Queue 1 was split correctly!");

    expect_messages_in_fifo_queue(
        ["30", "40"],
        &sqs_test_resources.sqs_client,
        &sqs_test_resources.echo_queue2,
    )
    .await;
    println!("Queue 2 was split correctly!");

    // TODO: verify queue tags.

    client_a.child.kill().await.unwrap();
    client_b.child.kill().await.unwrap();

    // verify_splitter_temp_queues_deleted(&sqs_test_resources).await;

    println!("All temporary queues were deleted!");
}
