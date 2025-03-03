#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use core::time::Duration;

use fancy_regex::Regex;
use rstest::*;
use tempfile::{NamedTempFile, TempPath};

use crate::utils::{
    sqs_resources::{sqs_test_resources, SqsTestResources},
    Application, TestProcess,
};

fn sqs_splitting_config(filter: Regex) -> TempPath {
    let mut file = NamedTempFile::with_suffix(".json").unwrap();
    let config = serde_json::json!({
        "operator": true,
        "feature": {
            "split_queues": {
                "e2e-test-queue1": {
                    "queue_type": "SQS",
                    "message_filter": {
                        "client": filter.as_str(),
                    },
                },
                "e2e-test-queue2": {
                    "queue_type": "SQS",
                    "message_filter": {
                        "client": filter.as_str(),
                    },
                },
            },
        },
    });
    serde_json::to_writer(file.as_file_mut(), &config).unwrap();
    file.into_temp_path()
}

/// Verify that the test process printed all the expected messages, and none other.
///
/// The expected lines should all be unique in both arrays together (a line should not appear in
/// both arrays or in any array twice).
async fn expect_output_lines(
    expected_lines: &[&str],
    expected_in_order_lines: &[&str],
    test_process: &TestProcess,
) {
    let lines = test_process
        .await_exactly_n_lines(
            expected_lines.len() + expected_in_order_lines.len() + 2,
            Duration::from_secs(20),
        )
        .await;

    for expected_line in expected_lines {
        assert!(
            lines.contains(&expected_line.to_string()),
            "User a was expected to print {expected_line} but did not"
        );
    }

    let mut last_location = 0;
    for expected_line in expected_in_order_lines {
        let location = lines
            .iter()
            .position(|line| *line == *expected_line)
            .expect("A fifo message did not reach the correct user.");
        assert!(last_location <= location, "Fifo messages out of order!");
        last_location = location;
    }
}

/// Waits until the test process prints to its stdout that it reads from 2 queues.
async fn wait_until_reads_from_two_queues(test_process: &TestProcess, timeout: Duration) {
    tokio::time::timeout(timeout, async move {
        loop {
            let queues = test_process
                .get_stdout()
                .await
                .split('\n')
                .filter(|line| line.starts_with("Reading messages from queue "))
                .count();
            match queues {
                0..=1 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                2 => break,
                n => panic!("test process reads from {n} queues, something is not right"),
            }
        }
    })
    .await
    .unwrap()
}

/// Run 2 local applications with mirrord that both consume messages from the same 2 queues.
/// Use different message filters in their mirrord configurations.
/// Send messages to both queues, with different values of the "client" message attribute, so that
/// they reach the different clients (or the deployed application).
/// The local applications print the message they received to stdout, so read their output and
/// verify each of the local applications gets the messages it is supposed to get.
/// The remote application forwards the messages it receives to "echo" queues, so receive messages
/// from those queues and verify the remote application exactly the messages it was supposed to.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(360))]
pub async fn two_users(#[future] sqs_test_resources: SqsTestResources) {
    let sqs_test_resources = sqs_test_resources.await;
    let application = Application::RustSqs;

    println!("Starting first mirrord client");
    let client_a_config = sqs_splitting_config(Regex::new("^a$").unwrap());
    let client_a = application
        .run(
            &sqs_test_resources.deployment_target(),
            Some(sqs_test_resources.namespace()),
            None,
            Some(vec![(
                "MIRRORD_CONFIG_FILE",
                client_a_config.to_str().unwrap(),
            )]),
        )
        .await;
    wait_until_reads_from_two_queues(&client_a, Duration::from_secs(30)).await;
    println!("First mirrord client reads from 2 queues");

    println!("Starting second mirrord client");
    let client_b_config = sqs_splitting_config(Regex::new("^b$").unwrap());
    let client_b = application
        .run(
            &sqs_test_resources.deployment_target(),
            Some(sqs_test_resources.namespace()),
            None,
            Some(vec![(
                "MIRRORD_CONFIG_FILE",
                client_b_config.to_str().unwrap(),
            )]),
        )
        .await;
    wait_until_reads_from_two_queues(&client_b, Duration::from_secs(30)).await;
    println!("Second mirrord client reads from 2 queues");

    let non_fifo_messages = [
        ("1", "a"),
        ("2", "b"),
        ("3", "c"),
        ("4", "c"),
        ("5", "b"),
        ("6", "a"),
    ];
    for (body, attr_value) in non_fifo_messages {
        sqs_test_resources
            .queue1
            .send_message(body, "client", attr_value)
            .await;
    }

    let fifo_messages = &[
        ("10", "a"),
        ("20", "b"),
        ("30", "c"),
        ("40", "c"),
        ("50", "b"),
        ("60", "a"),
    ];
    for (body, attr_value) in fifo_messages {
        sqs_test_resources
            .queue2
            .send_message(body, "client", attr_value)
            .await;
    }

    // Test app prints 1: before messages from queue 1 and 2: before messages from queue 2.
    expect_output_lines(&["1:1", "1:6"], &["2:10", "2:60"], &client_a).await;
    println!("Client a received the correct messages.");
    expect_output_lines(&["1:2", "1:5"], &["2:20", "2:50"], &client_b).await;
    println!("Client b received the correct messages.");

    sqs_test_resources
        .echo_queue1
        .expect_messages(vec!["3", "4"])
        .await;
    println!(
        "Queue 1 ({}) was split correctly!",
        sqs_test_resources.echo_queue1.name()
    );

    for message in ["30", "40"] {
        sqs_test_resources
            .echo_queue2
            .expect_messages(vec![message])
            .await;
    }
    println!(
        "Queue 2 ({}) was split correctly!",
        sqs_test_resources.echo_queue2.name()
    );

    // TODO: verify queue tags.
}
