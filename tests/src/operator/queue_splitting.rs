#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

mod test_resources;

use std::path::PathBuf;

use rstest::*;

use crate::{
    operator::queue_splitting::test_resources::{sqs_test_resources, SqsTestResources},
    utils::{config_dir, Application},
};

/// Send 6 messages to the original queue, such that 2 will reach each mirrord run and 2 the
/// deployed app.
async fn write_sqs_messages(queue_url: &str) {
    // TODO
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

    let mut client_b = application
        .run(
            &sqs_test_resources.deployment_target(),
            Some(sqs_test_resources.namespace()),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    // write_sqs_messages(&sqs_queue_url).await;

    // // The test application consumes messages and verifies exact expected messages.
    // join!(
    //     client_a.wait_assert_success(),
    //     client_b.wait_assert_success()
    // );

    // TODO: read output queue and verify that exactly the expected messages were
    //   consumed and forwarded.
}
