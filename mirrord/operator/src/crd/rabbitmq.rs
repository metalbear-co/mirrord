use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    ops::Not,
};

use amq_protocol_types::FieldTable;
use base64::{Engine, engine::general_purpose};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de, ser};
use tcp_stream::{OwnedIdentity, OwnedTLSConfig};
use url::Url;

use super::{QueueConsumer, QueueId, QueueMessageFilter, QueueNameUpdate, SplitQueueNameDetails};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")] // name_source -> nameSource in yaml.
pub struct RmqQueueDetails {
    /// the name of [`MirrordPropertyList`](crate::crd::properties::MirrordPropertyList) that
    /// contains the RabbitMQ cluster definition.
    pub cluster_ref: String,

    #[serde(flatten)]
    pub name_details: SplitQueueNameDetails,

    #[serde(default, skip_serializing_if = "Not::not")]
    pub durable: bool,

    #[serde(default, skip_serializing_if = "Not::not")]
    pub exclusive: bool,

    #[serde(default, skip_serializing_if = "Not::not")]
    pub auto_delete: bool,

    /// RabbitMQ specific arguments that will be used during for the cosume call.
    #[serde(default)]
    pub arguments: FieldTable,
}

impl Eq for RmqQueueDetails {}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename = "RMQSplitDetails", rename_all = "camelCase")]
pub struct RmqSplitDetails {
    /// Queue ID -> old and new queue names.
    pub queue_names: BTreeMap<QueueId, QueueNameUpdate>,

    // A bit redundant, because the registry resource status has the mapping from env var name
    // to queue id, and `queue_names` has the mapping from queue id to name update, but, saving
    // it here in the form that is useful to reader, for simplicity and readability.
    /// Env var name -> old and new queue names.
    pub env_updates: BTreeMap<String, QueueNameUpdate>,
}

/// Representation of Sqs errors for the status of RMQ session resources.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RmqSessionError {
    /// HTTP code for operator response.
    pub status_code: u16,

    /// Human-readable explanation of what went wrong.
    pub reason: String,
}

impl fmt::Display for RmqSessionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        // Write strictly the first element into the supplied output
        // stream: `f`. Returns `fmt::Result` which indicates whether the
        // operation succeeded or failed. Note that `write!` uses syntax which
        // is very similar to `println!`.
        write!(f, "{}", self.reason)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename = "RMQSessionStatus")]
pub enum RmqSessionStatus {
    // kube-rs does not allow mixing unit variants with tuple/struct variants, so this variant
    // has to be a tuple/struct too. If we leave the tuple empty, k8s complains about an object
    // without any items, and kube-rs does not support internally tagged enums, so we actually
    // have to put something in there, even if we don't actually care about that info.
    Starting {
        start_time_utc: String,
    },
    /// RMQ operator sets this status before it starts registering filters, so that if anything
    /// fails during the registration of filters, we have all the queues we need to delete on
    /// cleanup.
    RegisteringFilters(RmqSplitDetails),
    Ready(RmqSplitDetails),
    StartError(RmqSessionError),
    CleanupError {
        error: RmqSessionError,
        details: Option<RmqSplitDetails>,
    },
}

impl RmqSessionStatus {
    pub fn get_split_details(&self) -> Option<&RmqSplitDetails> {
        match self {
            RmqSessionStatus::RegisteringFilters(details) | RmqSessionStatus::Ready(details) => {
                Some(details)
            }
            RmqSessionStatus::CleanupError { details, .. } => details.as_ref(),
            _ => None,
        }
    }
}

/// The [`kube::runtime::wait::Condition`] trait is auto-implemented for this function.
/// To be used in [`kube::runtime::wait::await_condition`].
pub fn is_session_ready(session: Option<&MirrordRmqSession>) -> bool {
    session
        .and_then(|session| session.status.as_ref())
        .map(|status| {
            matches!(
                status,
                RmqSessionStatus::Ready(..)
                    | RmqSessionStatus::StartError(..)
                    | RmqSessionStatus::CleanupError { .. }
            )
        })
        .unwrap_or_default()
}

/// The operator creates this object when a user runs mirrord against a target that is a queue
/// consumer.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordRMQSession",
    root = "MirrordRmqSession", // for Rust naming conventions (Rmq, not RMQ)
    status = "RmqSessionStatus"
)]
#[serde(rename_all = "camelCase")] // queue_filters -> queueFilters
pub struct MirrordRmqSessionSpec {
    /// The associated [`MirrordPropertyList`](crate::crd::properties::MirrordPropertyList) name
    /// that contains the RabbitMQ cluster definition.
    pub cluster_ref: String,

    /// Kubernetes namespace for the session (in workload clusters)
    pub namespace: String,

    /// For each queue_id, a mapping from attribute name, to attribute value regex.
    /// The queue_id for a queue is determined at the queue registry. It is not (necessarily)
    /// The name of the queue on AWS.
    pub queue_filters: HashMap<QueueId, QueueMessageFilter>,

    /// The target of this session.
    pub queue_consumer: QueueConsumer,

    /// The id of the mirrord exec session, from the operator.
    // The Kubernetes API can't deal with 64 bit numbers (with most significant bit set)
    // so we save that field as a (HEX) string even though its source is a u64
    pub session_id: String,

    /// Multi-cluster coordination: explicit output queue names.
    ///
    /// Maps original queue names to their corresponding output queue names.
    /// For multi-cluster: the default cluster creates temp queues and passes the exact names here.
    /// Other clusters use these names directly instead of generating their own.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output_queue_names: HashMap<String, String>,
}
