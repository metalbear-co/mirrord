use std::collections::{BTreeSet, HashMap, HashSet};

use mirrord_config::{LayerConfig, feature::split_queues::QueueKind};
use mirrord_progress::{IdeAction, IdeMessage, NotificationLevel, Progress, utm_medium};
use strum::IntoEnumIterator;

use crate::CliResult;

/// Landing page for the queue splitting documentation.
const QUEUE_SPLITTING_DOCS: &str =
    "https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting";

/// Notification id, mapped to the `promptQueueSplitting` config entry in the vscode extension.
const QUEUE_SPLITTING_HINT_ID: &str = "queue_splitting_hint";

/// Detects queue kinds hinted at by the target's environment.
///
/// Tokens are matched case-insensitively as whole words in both env keys and values.
fn detect_queue_kinds(env: &HashMap<String, String>) -> BTreeSet<QueueKind> {
    QueueKind::iter()
        .filter(|kind| {
            let tokens: &[&str] = match kind {
                QueueKind::Sqs => &["sqs"],
                QueueKind::Kafka => &["kafka"],
                QueueKind::Rmq => &["rabbitmq", "rmq", "amqp"],
                QueueKind::GcpPubSub => &["pubsub"],
                QueueKind::RedisPubSub => &["redis"],
                QueueKind::AzureServiceBus => &["servicebus"],
                QueueKind::Temporal => &["temporal"],
                QueueKind::BullMq => &["bullmq"],
                QueueKind::Unknown => &[],
            };

            tokens.iter().any(|token| {
                env.iter().any(|(key, value)| {
                    contains_whole_word(key, token) || contains_whole_word(value, token)
                })
            })
        })
        .collect()
}

/// Whether `token` appears in `haystack` as a whole word, case-insensitively.
///
/// Words are maximal runs of ASCII alphanumerics.
fn contains_whole_word(haystack: &str, token: &str) -> bool {
    haystack
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case(token))
}

/// Nudges the user toward queue splitting when the target uses any supported queues.
///
/// Does nothing when queue splitting is already configured or no supported queue is detected.
pub fn suggest_queue_splitting<P: Progress>(
    config: &LayerConfig,
    env: &HashMap<String, String>,
    uses_operator: bool,
    progress: &mut P,
) -> CliResult<()> {
    if config.feature.split_queues.is_set() {
        return Ok(());
    }

    let detected = detect_queue_kinds(env)
        .iter()
        .filter_map(|kind| match kind {
            QueueKind::Sqs => Some("Amazon SQS"),
            QueueKind::Kafka => Some("Kafka"),
            QueueKind::Rmq => Some("RabbitMQ"),
            QueueKind::GcpPubSub => Some("GCP Pub/Sub"),
            QueueKind::RedisPubSub => Some("Redis Pub/Sub"),
            QueueKind::AzureServiceBus => Some("Azure Service Bus"),
            QueueKind::Temporal => Some("Temporal"),
            QueueKind::BullMq => Some("BullMQ"),
            QueueKind::Unknown => None,
        })
        .collect::<Vec<_>>();

    let Some((last, rest)) = detected.split_last() else {
        return Ok(());
    };

    let names = match rest {
        [] => last.to_string(),
        [single] => format!("{single} and {last}"),
        more => format!("{}, and {}", more.join(", "), last),
    };

    let (offer, cta) = if uses_operator {
        ("mirrord can split these queues", "Set it up")
    } else {
        (
            "With mirrord for Teams you can split these queues",
            "Learn more",
        )
    };

    let text = format!(
        "mirrord detected that your target uses {names}. {offer}, so your local run only receives \
the messages you filter for while your teammates keep getting theirs."
    );

    let mut actions = HashSet::new();

    actions.insert(IdeAction::Link {
        label: "Queue splitting docs".to_owned(),
        link: format!("{QUEUE_SPLITTING_DOCS}?utm_medium={}", utm_medium()),
    });

    progress.ide(serde_json::to_value(IdeMessage {
        id: QUEUE_SPLITTING_HINT_ID.to_owned(),
        level: NotificationLevel::Info,
        text: text.clone(),
        actions,
    })?);

    progress.add_to_print_buffer(&format!("\n\n{text}\n>> {cta}: {QUEUE_SPLITTING_DOCS}\n"));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_matching() {
        assert!(contains_whole_word("KAFKA_BROKERS", "kafka"));
        assert!(contains_whole_word("sqs.us-east-1.amazonaws.com", "sqs"));
        assert!(contains_whole_word("my_sqs", "sqs"));
        assert!(contains_whole_word("SQS", "sqs"));
        assert!(!contains_whole_word("kafkaesque", "kafka"));
        assert!(!contains_whole_word("bullmq1337", "bullmq"));
        assert!(!contains_whole_word("abcsqsabc", "sqs"));
        assert!(!contains_whole_word("", "sqs"));
    }

    #[test]
    fn detects_from_keys_and_values() {
        let env = HashMap::from([
            (
                "KAFKA_BOOTSTRAP_SERVERS".to_owned(),
                "broker:9092".to_owned(),
            ),
            (
                "QUEUE_URL".to_owned(),
                "https://sqs.us-east-1.amazonaws.com/1/orders".to_owned(),
            ),
            ("DATABASE_URL".to_owned(), "postgres://db".to_owned()),
        ]);

        let detected = detect_queue_kinds(&env);

        assert_eq!(detected, BTreeSet::from([QueueKind::Sqs, QueueKind::Kafka]));
    }

    #[test]
    fn ignores_substring_false_positives() {
        let env = HashMap::from([(
            "DESCRIPTION".to_owned(),
            "a kafkaesque bureaucracy".to_owned(),
        )]);

        assert!(detect_queue_kinds(&env).is_empty());
    }
}
