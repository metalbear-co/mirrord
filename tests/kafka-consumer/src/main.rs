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
    /// YAML file to read the topic and group from, overriding the env values.
    /// Mirrors apps that keep queue names in a mounted config file instead of
    /// env vars - under mirrord the read goes through the remote fs, so tests
    /// exercise the operator's file-content overrides with a real consumer.
    #[serde(rename = "config_file", default)]
    file: Option<String>,
    /// Dot path to the topic name inside the config file.
    #[serde(rename = "config_topic_path", default = "Config::default_topic_path")]
    topic_path: String,
    /// Dot path to the consumer group inside the config file.
    #[serde(rename = "config_group_path", default = "Config::default_group_path")]
    group_path: String,
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

    fn default_topic_path() -> String {
        ".kafka.consumer.topic.main.name".into()
    }

    fn default_group_path() -> String {
        ".kafka.consumer.group".into()
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

/// Walks `doc` along a `.a.b.c` dot path and returns the string at the end.
fn select_yaml(doc: &serde_yaml::Value, path: &str) -> anyhow::Result<String> {
    let mut current = doc;
    for part in path
        .trim_start_matches('.')
        .split('.')
        .filter(|p| !p.is_empty())
    {
        current = current
            .get(part)
            .with_context(|| format!("key `{part}` of path `{path}` not found in config file"))?;
    }
    current
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("value at `{path}` is not a string"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut config = Figment::new()
        .merge(Env::raw())
        .extract::<Config>()
        .context("failed to read configuration")?;

    if let Some(path) = &config.file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file `{path}`"))?;
        let doc: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse config file `{path}` as YAML"))?;
        config.topic = select_yaml(&doc, &config.topic_path)?;
        config.group = select_yaml(&doc, &config.group_path)?;
    }

    // Stderr on purpose: every stdout line is parsed as a received message by
    // the test harness.
    eprintln!(
        "resolved config: topic={} group={} address={}",
        config.topic, config.group, config.address,
    );

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &config.address)
        .set("group.id", &config.group)
        .set("enable.auto.commit", "false")
        // This consumer joins the target workload's group, so its assignment waits on the same
        // rebalance the operator's forwarder does - up to `session.timeout.ms` while the restarted
        // target is evicted. The forwarder writes into the split topic as soon as it is assigned,
        // which can land before this consumer is. librdkafka's default "latest" would then start
        // past those messages and never deliver them; "earliest" reads the split topic from the
        // start. The topic is created per session, so that is exactly this session's messages.
        .set("auto.offset.reset", "earliest")
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
