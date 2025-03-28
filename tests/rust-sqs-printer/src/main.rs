use aws_sdk_sqs::{operation::receive_message::ReceiveMessageOutput, types::Message, Client};
use tokio::time::{sleep, Duration};

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// URL of the environment variable that holds the name of the second SQS queue to read from.
/// Using URL here and not name, to also test that functionality.
const QUEUE2_URL_ENV_VAR: &str = "SQS_TEST_Q2_URL";

/// Reads from queue and prints the contents of each message in a new line.
async fn read_from_queue_by_name(read_q_name: String, client: Client, queue_num: u8) {
    let read_q_url = client
        .get_queue_url()
        .queue_name(read_q_name)
        .send()
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
        let res = match receive_message_request.clone().send().await {
            Ok(res) => res,
            Err(err) => {
                println!("ERROR: {err:?}");
                sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        if let ReceiveMessageOutput {
            messages: Some(messages),
            ..
        } = res
        {
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
                    client
                        .delete_message()
                        .queue_url(&read_q_url)
                        .receipt_handle(handle)
                        .send()
                        .await
                        .unwrap();
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let sdk_config = aws_config::load_from_env().await;
    let client = Client::new(&sdk_config);
    let read_q_name = std::env::var(QUEUE_NAME_ENV_VAR1).unwrap();
    let q_task_handle = tokio::spawn(read_from_queue_by_name(
        read_q_name.clone(),
        client.clone(),
        1,
    ));
    let read_q_url = std::env::var(QUEUE2_URL_ENV_VAR).unwrap();
    let fifo_q_task_handle = tokio::spawn(read_from_queue_by_url(
        read_q_url.clone(),
        client.clone(),
        2,
    ));
    let (q_res, fifo_res) = tokio::join!(q_task_handle, fifo_q_task_handle);
    q_res.unwrap();
    fifo_res.unwrap();
}
