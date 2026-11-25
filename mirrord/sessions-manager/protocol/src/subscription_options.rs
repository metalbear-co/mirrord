//! What a registration asks of the sessions-manager beyond its identity: which tasks a developer
//! wants, and what an agent knows about its task.
//!
//! Both sides send these in [`SUBSCRIPTION_OPTIONS_HEADER`] on the existing assignments
//! subscription, as base64url JSON, and leave the query as it is. Filters and tags describe the
//! customer's workload, and URLs end up in access logs, so they never travel in the query. A
//! client that has nothing to ask sends no header, which is how every client registered before
//! the header existed.
//!
//! Only the operator-hosted sessions-manager reads the header. It looks up a task's tags itself
//! from what the agent sends ([`AgentSubscriptionOptions::aws_batch`]), and selects tasks for a
//! developer by matching those tags against [`AgentSelector`]. A [`Watch`] turns a developer's
//! registration into one that receives every matching task, and with [`Watch::replace`] the
//! developer's local process runs in place of the task's own code.

use std::{collections::BTreeMap, fmt};

use base64::{
    Engine,
    alphabet::URL_SAFE,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Header carrying the encoded [`IntproxySubscriptionOptions`] or [`AgentSubscriptionOptions`] of
/// an assignments subscription.
///
/// kube-apiserver forwards it unchanged to the operator's aggregated APIService, as it does
/// [`OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER`](crate::OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER).
pub const SUBSCRIPTION_OPTIONS_HEADER: &str = "x-mirrord-subscription-options";

/// Writes base64url without padding, and reads it with or without.
const OPTIONS_ENGINE: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

mod sealed {
    pub trait Sealed {}

    impl Sealed for super::IntproxySubscriptionOptions {}
    impl Sealed for super::AgentSubscriptionOptions {}
}

/// The options one side of an assignments subscription sends in [`SUBSCRIPTION_OPTIONS_HEADER`],
/// with their encoding.
///
/// Sealed, so that [`Self::encode`] only ever serializes the plain structs of this module, which
/// can't fail.
pub trait SubscriptionOptions: Serialize + DeserializeOwned + sealed::Sealed {
    /// The value of [`SUBSCRIPTION_OPTIONS_HEADER`] for these options.
    fn encode(&self) -> String {
        // The implementors are structs of strings, booleans and maps with string keys, which
        // serde_json always serializes.
        let json = serde_json::to_vec(self).expect("subscription options serialize to JSON");
        OPTIONS_ENGINE.encode(json)
    }

    /// Reads the options from the value of [`SUBSCRIPTION_OPTIONS_HEADER`].
    fn decode(value: &[u8]) -> Result<Self, InvalidSubscriptionOptions> {
        let json = OPTIONS_ENGINE
            .decode(value)
            .map_err(InvalidSubscriptionOptions::Base64)?;
        serde_json::from_slice(&json).map_err(InvalidSubscriptionOptions::Json)
    }
}

impl SubscriptionOptions for IntproxySubscriptionOptions {}
impl SubscriptionOptions for AgentSubscriptionOptions {}

/// Why [`SubscriptionOptions::decode`] refused a header value. Caused by the sender.
#[derive(Debug, thiserror::Error)]
pub enum InvalidSubscriptionOptions {
    #[error("the `{SUBSCRIPTION_OPTIONS_HEADER}` header is not base64url")]
    Base64(#[source] base64::DecodeError),
    /// The JSON error names the field that is unknown or malformed, which is what the sender
    /// needs to hear.
    #[error(transparent)]
    Json(serde_json::Error),
}

/// What a developer's registration (an intproxy subscription, or `mirrord up` watching a
/// service) asks for beyond pairing with any agent of its scope.
///
/// A sessions-manager refuses options with a field it doesn't know, instead of ignoring it: every
/// field changes which tasks the developer gets or what happens to them, and a client has no
/// other way to learn that an older sessions-manager did not honor one. A client therefore leaves
/// out the fields it doesn't use, and gates the ones it does on an operator feature.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntproxySubscriptionOptions {
    /// Restricts the agents this registration may be paired with to those whose task matches.
    ///
    /// Among several matching agents, a pairing takes the one with the lowest replica id. An
    /// agent whose tags are unknown (its companion sent nothing to look them up by, or the lookup
    /// failed) never matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_selector: Option<AgentSelector>,

    /// Makes this registration a watch: it is never paired with an agent, and instead receives a
    /// [`TaskMatch`](crate::TaskMatch) for every agent matching [`Self::agent_selector`], and a
    /// [`TaskEnded`](crate::TaskEnded) when that agent goes away. The developer then connects to
    /// each matched task with a registration of its own, pinned to the task's replica with
    /// `agent_replica_filter`.
    ///
    /// Every watch needs an [`Self::agent_selector`], whether it replaces tasks or not: the
    /// sessions-manager refuses a watch without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<Watch>,
}

/// Selects the tasks of a service by their tags, as queue splitting selects messages by their
/// attributes.
///
/// With both parts, a task must match both. With neither, it matches no task.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSelector {
    /// Tag names mapped to regexes. A task matches when it has every named tag, with a value its
    /// regex matches. An empty map matches every task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_filter: Option<BTreeMap<String, String>>,

    /// A jq program run on `{"tags": {"<name>": "<value>", ...}}`. A task matches when the
    /// program outputs `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq_filter: Option<String>,
}

/// Stands in for filter contents in [`fmt::Debug`] output. Filters describe the customer's
/// workload, so they stay out of logs.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Debug for AgentSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentSelector")
            .field("tag_filter", &self.tag_filter.as_ref().map(|_| Redacted))
            .field("jq_filter", &self.jq_filter.as_ref().map(|_| Redacted))
            .finish()
    }
}

/// How a watching registration treats the tasks it matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    /// The developer's local process runs in place of each matched task's own code, and the
    /// task ends with that process's exit code.
    ///
    /// Only tasks whose agent registered with [`GateState::Pending`] can be replaced, because
    /// their code hasn't started yet, and each one goes to the oldest replace watch whose
    /// [`IntproxySubscriptionOptions::agent_selector`] matches it. The watch's developer reports
    /// the local exit code with `mirrord_protocol::codec::ClientMessage::ReplacedTaskExit` on a
    /// connection paired with the task.
    ///
    /// Without it, matched tasks run their own code, and the watch only learns about them.
    #[serde(default)]
    pub replace: bool,
}

/// What an agent's registration tells the sessions-manager about the task it serves.
///
/// Read leniently, ignoring fields it doesn't know and reading a [`GateState`] it doesn't know
/// as [`GateState::Unknown`]: what an agent says about its task is information, not a request,
/// and a newer agent must keep working with an older operator.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSubscriptionOptions {
    /// The AWS Batch job the agent's task belongs to. The operator reads the job's tags with
    /// `DescribeJobs` under its own role, so the agent never calls AWS and the tags can't be
    /// forged by the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_batch: Option<AwsBatchJob>,

    /// The state of the task's admission gate, for a task whose application waits before `main`
    /// for a verdict. Absent for a task without a gate, or one told to run its own code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateState>,
}

/// Identifies an AWS Batch job, as its container sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwsBatchJob {
    /// From `AWS_BATCH_JOB_ID`. For a child of an array job, it carries the child's index.
    pub job_id: String,
    /// The region the job runs in, which selects the endpoint `DescribeJobs` is sent to.
    pub region: String,
}

/// The admission gate of a task whose application waits before `main`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    /// The application waits for a verdict, which the sessions-manager sends as an
    /// [`Admission`](crate::Admission) event on this subscription.
    Pending,
    /// The task was replaced: its own code won't run. Sent by an agent registering again, for
    /// example after the operator restarted, so the task stays out of every pairing that doesn't
    /// name its replica.
    Held,
    /// A state this version doesn't know, sent by a newer agent. Read as no gate at all: the
    /// sessions-manager sends no verdict, and the task decides on its own, as it does with an
    /// operator that predates admission.
    #[serde(other)]
    Unknown,
}
