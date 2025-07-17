use std::{collections::HashMap, fmt, future::Future};

use aws_sdk_sqs::{error::SdkError, types::Message, Client};
use tokio::time::{sleep, Duration};

/// Backoff used in case of IO errors.
const BACKOFF: Duration = Duration::from_secs(2);

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// URL of the environment variable that holds the name of the second SQS queue to read from.
/// Using URL here and not name, to also test that functionality.
const QUEUE2_URL_ENV_VAR: &str = "SQS_TEST_Q2_URL";

/// Name of the environment variable that holds a json object with the names/urls of the queues to
/// use. If this env var is set, the application will use the names from the json and will ignore
/// the two env vars.
const QUEUE_JSON_ENV_VAR: &str = "SQS_TEST_Q_JSON";

/// The keys inside the json object.
const QUEUE1_NAME_KEY: &str = "queue1_name";
const QUEUE2_URL_KEY: &str = "queue2_url";

/// In a loop, retries the future produced by the given function,
/// until it succeeds or returns a fatal error.
///
/// Some errors are retried with backoff. These are errors that can be triggered by agent loss,
/// which can happen when the agent is restarting the target workload.
async fn retry_with_backoff<F, Fut, T, E, R>(mut f: F) -> Result<T, SdkError<E, R>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, SdkError<E, R>>>,
    E: fmt::Debug,
    R: fmt::Debug,
{
    loop {
        match f().await {
            Ok(res) => break Ok(res),
            Err(err @ SdkError::DispatchFailure(..) | err @ SdkError::ResponseError(..)) => {
                // This might be triggered by agent loss, so we retry with backoff.
                eprintln!("SQS request failed, retrying with backoff: {err:?}");
                sleep(BACKOFF).await;
            }
            Err(err) => break Err(err),
        }
    }
}

/// Reads from queue and prints the contents of each message in a new line.
async fn read_from_queue_by_name(read_q_name: String, client: Client, queue_num: u8) {
    let read_q_url = retry_with_backoff(|| client.get_queue_url().queue_name(&read_q_name).send())
        .await
        .unwrap()
        .queue_url
        .unwrap();

    read_from_queue_by_url(read_q_url, client, queue_num).await;
}

/// Reads from queue and prints the contents of each message in a new line.
async fn read_from_queue_by_url(read_q_url: String, client: Client, queue_num: u8) {
    let receive_message_request = client
        .receive_message()
        .message_attribute_names(".*")
        // Without this the wait time would be 0 and responses would return immediately also when
        // there are no messages (and potentially even sometimes return empty immediately when
        // there are actually messages).
        // By setting a time != 0 (20 is the maximum), we perform "long polling" which means we
        // won't get "false empties" and also less empty responses, because SQS will wait for that
        // time before returning an empty response.
        .wait_time_seconds(5)
        .queue_url(&read_q_url);
    loop {
        let messages = retry_with_backoff(|| receive_message_request.clone().send())
            .await
            .unwrap()
            .messages
            .unwrap_or_default();
        for Message {
            body,
            receipt_handle,
            ..
        } in messages
        {
            println!(
                "{queue_num}:{}",
                body.expect("Got message without body. Expected content.")
            );

            if let Some(handle) = receipt_handle {
                retry_with_backoff(|| {
                    client
                        .delete_message()
                        .queue_url(&read_q_url)
                        .receipt_handle(&handle)
                        .send()
                })
                .await
                .unwrap();
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let sdk_config = aws_config::load_from_env().await;
    let client = Client::new(&sdk_config);

    let (q1_name, q2_url) = match std::env::var(QUEUE_JSON_ENV_VAR) {
        Ok(q_json_string) => {
            // The json env var was set, in this case the app gets the queues by parsing a json
            // that is in the value of that env var.
            let mut q_map: HashMap<String, String> = serde_json::from_str(&q_json_string).unwrap();
            let q1_name = q_map.remove(QUEUE1_NAME_KEY).unwrap();
            let q2_url = q_map.remove(QUEUE2_URL_KEY).unwrap();
            (q1_name, q2_url)
        }
        Err(_) => {
            // The json env var was not set. In this case this app gets each queue name/url from
            // its own env var.
            let q1_name = std::env::var(QUEUE_NAME_ENV_VAR1).unwrap();
            let q2_url = std::env::var(QUEUE2_URL_ENV_VAR).unwrap();
            (q1_name, q2_url)
        }
    };

    let q_task_handle = tokio::spawn(read_from_queue_by_name(q1_name, client.clone(), 1));
    let fifo_q_task_handle = tokio::spawn(read_from_queue_by_url(q2_url, client.clone(), 2));
    let (q_res, fifo_res) = tokio::join!(q_task_handle, fifo_q_task_handle);
    q_res.unwrap();
    fifo_res.unwrap();
}
