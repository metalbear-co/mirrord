use std::{collections::BTreeMap, io::Write};

use anyhow::Context;
use figment::{Figment, providers::Env};
use rdkafka::{
    ClientConfig,
    consumer::{Consumer, StreamConsumer},
    message::{Headers, Message},
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Config {
    #[serde(default = "Config::default_address")]
    address: String,
    #[serde(alias = "kafka_group.id", default = "Config::default_group")]
    group: String,
    #[serde(alias = "input_topic_1", default = "Config::default_topic")]
    topic: String,
}

impl Config {
    fn default_address() -> String {
        "my-cluster-kafka-bootstrap:9092".into()
    }

    fn default_group() -> String {
        "my-group".into()
    }

    fn default_topic() -> String {
        "my-topic".into()
    }
}

#[derive(Serialize)]
struct Output<'a> {
    topic: &'a str,
    offset: i64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<&'a str, &'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<&'a str>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Figment::new()
        .merge(Env::raw())
        .extract::<Config>()
        .context("failed to read configuration")?;

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &config.address)
        .set("group.id", &config.group)
        .set("enable.auto.commit", "false")
        // Allow fetching messages larger than the librdkafka 1 MB default so the oversized-message
        // test can pull a payload above 1 MB from the split topic.
        .set("fetch.message.max.bytes", "10485760")
        .set("message.max.bytes", "10485760")
        .create()
        .context("failed to create consumer")?;

    consumer
        .subscribe(&[&config.topic])
        .context("failed to subscribe to topic")?;

    let mut stdout = std::io::stdout();

    loop {
        let msg = consumer.recv().await.context("failed to receive message")?;

        serde_json::to_writer(
            &mut stdout,
            &Output {
                topic: msg.topic(),
                offset: msg.offset(),
                headers: msg
                    .headers()
                    .map(|headers| {
                        headers
                            .iter()
                            .filter_map(|header| {
                                Some((
                                    header.key,
                                    header.value.and_then(|value| str::from_utf8(value).ok())?,
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                payload: msg.payload_view::<str>().and_then(Result::ok),
            },
        )
        .context("failed to serialize output")?;

        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
}
