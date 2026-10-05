//! The `generate_config` tool: writes a `mirrord.json` or `mirrord-up.yaml` from structured
//! options, so an agent describes what it wants instead of composing the file by hand.
//!
//! The file is built as a JSON value holding only the options that were given, and every result is
//! checked with [`validate_config`] before it is returned: the tool refuses rather than hand back a
//! config mirrord would reject. Object keys come out sorted (`serde_json::Map` is a `BTreeMap`)
//! and lists keep the order they were given in, so the same options always give the same bytes.

use std::{collections::BTreeMap, ops::Not};

use mirrord_config::feature::{fs::FsModeConfig, network::incoming::IncomingMode};
use mirrord_up::{RunType, ServiceMode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strum_macros::IntoStaticStr;
use thiserror::Error;

use super::validate_config::{
    ConfigFormat, ConfigIssue, ValidateConfigArgs, ValidateConfigError, escape_pointer_token,
    validate_config,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerateConfigArgs {
    /// Which config file to generate; defaults to `mirrord.json`.
    #[serde(default)]
    pub format: Option<ConfigFormat>,
    /// The options of a `mirrord.json`. Only for `format: mirrord.json`.
    #[serde(default)]
    pub config: Option<ConfigOptions>,
    /// Settings shared by every service. Only for `format: mirrord-up.yaml`.
    #[serde(default)]
    pub common: Option<CommonOptions>,
    /// The services `mirrord up` runs. Required for `format: mirrord-up.yaml`.
    #[serde(default)]
    pub services: Option<Vec<ServiceOptions>>,
}

/// The options of a single mirrord session. Options that are not given are left out of the file,
/// so mirrord's defaults apply to them.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigOptions {
    /// The Kubernetes resource the local process runs in the context of.
    #[serde(default)]
    pub target: Option<TargetOptions>,
    /// Traffic coming into the target.
    #[serde(default)]
    pub incoming: Option<IncomingOptions>,
    /// Traffic going out of the local process.
    #[serde(default)]
    pub outgoing: Option<OutgoingOptions>,
    /// DNS resolution of the local process.
    #[serde(default)]
    pub dns: Option<DnsOptions>,
    /// Environment variables taken from the target.
    #[serde(default)]
    pub env: Option<EnvOptions>,
    /// File operations of the local process.
    #[serde(default)]
    pub fs: Option<FsOptions>,
    /// Run in a copy of the target instead of the target itself. Needs the mirrord Operator.
    #[serde(default)]
    pub copy_target: Option<CopyTargetOptions>,
    /// Queues whose messages are split between the local process and the target. Needs the
    /// mirrord Operator.
    #[serde(default)]
    pub split_queues: Option<Vec<QueueSplitOptions>>,
    /// The namespace the mirrord agent is created in.
    #[serde(default)]
    pub agent_namespace: Option<String>,
    /// The kubeconfig context to use.
    #[serde(default)]
    pub kube_context: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetOptions {
    pub r#type: TargetType,
    /// The name of the resource. Required unless `type` is `targetless`.
    #[serde(default)]
    pub name: Option<String>,
    /// The container to target, when the pod has more than one.
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema, IntoStaticStr)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum TargetType {
    /// No target: the local process runs in the cluster's network, without a pod's context.
    Targetless,
    Pod,
    Deployment,
    Rollout,
    /// Needs the mirrord Operator.
    Job,
    /// Needs the mirrord Operator.
    CronJob,
    /// Needs the mirrord Operator.
    StatefulSet,
    /// Needs the mirrord Operator.
    Service,
    /// Needs the mirrord Operator.
    ReplicaSet,
}

impl TargetType {
    fn needs_operator(self) -> bool {
        matches!(
            self,
            Self::Job | Self::CronJob | Self::StatefulSet | Self::Service | Self::ReplicaSet
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IncomingOptions {
    /// `mirror` copies incoming traffic to the local process, `steal` redirects it there, `off`
    /// leaves it alone. Not for `mirrord-up.yaml` services, whose `mode` sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<IncomingMode>,
    /// Only steal the HTTP requests matching this filter, letting the rest reach the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<HttpFilterOptions>,
    /// The ports to mirror or steal; all ports when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<u16>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpFilterOptions {
    /// Regex matched against each request header, as `name: value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_filter: Option<String>,
    /// Regex matched against the request path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_filter: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutgoingOptions {
    /// Send outgoing TCP from the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp: Option<bool>,
    /// Send outgoing UDP from the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AddressFilter>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DnsOptions {
    /// Resolve names in the cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AddressFilter>,
}

/// Which addresses go through the cluster: only the `remote` ones, or all but the `local` ones.
/// Exactly one of the two is given. Each entry is a `host:port`, `host`, `:port` or a CIDR,
/// optionally prefixed with `tcp://` or `udp://`.
// A struct rather than an externally tagged enum, which is what the config takes, so that giving
// both is refused with an explanation instead of a serde error about map lengths.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddressFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvOptions {
    /// Only take these variables from the target; `*` and `?` are wildcards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    /// Take every variable but these from the target; `*` and `?` are wildcards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Variables set to these values, whatever the target has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#override: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FsOptions {
    /// `read` reads files from the target and writes locally, `write` does both on the target,
    /// `local` does both locally, `localwithoverrides` is local except for the lists below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FsModeConfig>,
    /// Regexes of paths read and written on the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_write: Option<Vec<String>>,
    /// Regexes of paths read from the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<Vec<String>>,
    /// Regexes of paths opened locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<Vec<String>>,
    /// Regexes of paths reported as not existing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_found: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyTargetOptions {
    /// Scale the original workload down to zero while the copy runs, so the local process gets
    /// all of its traffic.
    #[serde(default)]
    pub scale_down: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueSplitOptions {
    /// The id of the queue in the operator's `MirrordWorkloadQueueRegistry` for the target.
    pub queue_id: String,
    pub queue_type: QueueType,
    /// Message attributes (SQS) or headers (Kafka) mapped to regexes. Messages matching all of
    /// them go to the local process.
    #[serde(default)]
    pub message_filter: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
pub enum QueueType {
    #[serde(rename = "SQS")]
    Sqs,
    Kafka,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommonOptions {
    /// The kubeconfig context every service uses unless it sets its own.
    #[serde(default)]
    pub kube_context: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceOptions {
    /// The service's key under `services`. When there is no `target`, `mirrord up` targets the
    /// workload with this name.
    pub name: String,
    pub run: RunOptions,
    /// How the service shares traffic with its target; `split` when omitted. `replace` needs the
    /// mirrord Operator.
    #[serde(default)]
    pub mode: Option<ServiceMode>,
    #[serde(default)]
    pub config: Option<ConfigOptions>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunOptions {
    /// The program and its arguments, e.g. `["npm", "run", "dev"]`.
    pub command: Vec<String>,
    /// `exec` runs `command` locally, `container` runs a command that starts a container;
    /// `exec` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<RunType>,
    /// The directory `command` runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GenerateConfigOutput {
    pub format: ConfigFormat,
    /// The file content, valid as is.
    pub content: String,
    /// JSON pointers to the options in `content` that need the mirrord Operator (a paid plan).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires_operator: Vec<String>,
}

#[derive(Debug, Error)]
pub enum GenerateConfigError {
    #[error("`config` only applies to `mirrord.json`; give `common` and `services` instead")]
    ConfigForUp,
    #[error("`common` and `services` only apply to `mirrord-up.yaml`; give `config` instead")]
    ServicesForJson,
    #[error("`mirrord-up.yaml` needs at least one service")]
    NoServices,
    #[error("service `{0}` is given more than once")]
    DuplicateService(String),
    #[error("a `targetless` target takes no `name` or `container`")]
    TargetlessWithName,
    #[error("a `{0}` target needs a `name`")]
    MissingTargetName(&'static str),
    #[error("the target's `{0}` is empty")]
    EmptyTargetField(&'static str),
    #[error("the target's `{0}` contains a `/`; give the container in `container`")]
    SlashInTargetField(&'static str),
    #[error("`{0}.filter` takes either `remote` or `local`, not both or neither")]
    AddressFilter(&'static str),
    #[error("a service needs a non-empty `name`")]
    EmptyServiceName,
    #[error("service `{0}` needs a non-empty `run.command`")]
    EmptyCommand(String),
    #[error(
        "service `{0}` sets `config.incoming.mode`; a service's incoming traffic is set by its \
        `mode` (`split` steals, `mirror` mirrors)"
    )]
    IncomingModeInService(String),
    #[error(
        "service `{0}` has an `incoming.http_filter` with `mode: replace`, which takes all \
        traffic and ignores the filter; use `split` or `mirror` to filter"
    )]
    HttpFilterWithReplace(String),
    #[error("queue `{0}` is given more than once in `split_queues`")]
    DuplicateQueue(String),
    #[error("the options give an invalid config: {}", describe(.0))]
    Invalid(Vec<ConfigIssue>),
    #[error(transparent)]
    Validate(#[from] ValidateConfigError),
    #[error("failed to write the config: {0}")]
    Serialize(String),
}

fn describe(issues: &[ConfigIssue]) -> String {
    issues
        .iter()
        .map(|issue| match issue.path.as_str() {
            "" => issue.message.clone(),
            path => format!("{path}: {}", issue.message),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Where `mirrord-up.yaml` service fields go in the `mirrord.json` that `ServiceConfig::assemble`
/// builds for the service. Options with no service field of their own go in its `config_patch`.
const SERVICE_FIELDS: &[(&str, &[&str])] = &[
    ("target", &["target"]),
    ("context", &["kube_context"]),
    ("env", &["feature", "env"]),
    (
        "http_filter",
        &["feature", "network", "incoming", "http_filter"],
    ),
];

pub fn generate_config(
    GenerateConfigArgs {
        format,
        config,
        common,
        services,
    }: GenerateConfigArgs,
) -> Result<GenerateConfigOutput, GenerateConfigError> {
    let format = format.unwrap_or(ConfigFormat::MirrordJson);
    let (content, requires_operator) = match format {
        ConfigFormat::MirrordJson => {
            if common.is_some() || services.is_some() {
                return Err(GenerateConfigError::ServicesForJson);
            }
            let (config, requires_operator) = layer_config(config.unwrap_or_default())?;
            let content = serde_json::to_string_pretty(&config)
                .map_err(|error| GenerateConfigError::Serialize(error.to_string()))?;
            (content + "\n", requires_operator)
        }
        ConfigFormat::MirrordUpYaml => {
            if config.is_some() {
                return Err(GenerateConfigError::ConfigForUp);
            }
            let (config, requires_operator) = up_config(common, services.unwrap_or_default())?;
            let content = serde_saphyr::to_string(&config)
                .map_err(|error| GenerateConfigError::Serialize(error.to_string()))?;
            (content, requires_operator)
        }
    };

    let validation = validate_config(ValidateConfigArgs {
        format,
        content: content.clone(),
        key: None,
    })?;
    if validation.valid.not() {
        return Err(GenerateConfigError::Invalid(validation.issues));
    }

    Ok(GenerateConfigOutput {
        format,
        content,
        requires_operator,
    })
}

/// Builds a `mirrord.json` from `options`, along with pointers to the options in it that need the
/// operator.
fn layer_config(
    options: ConfigOptions,
) -> Result<(Map<String, Value>, Vec<String>), GenerateConfigError> {
    let ConfigOptions {
        target,
        incoming,
        outgoing,
        dns,
        env,
        fs,
        copy_target,
        split_queues,
        agent_namespace,
        kube_context,
    } = options;

    let mut config = Map::new();
    let mut requires_operator = Vec::new();

    if let Some(target) = target {
        if target.r#type.needs_operator() {
            requires_operator.push("/target".to_owned());
        }
        config.insert("target".to_owned(), target_value(target)?);
    }
    set(&mut config, &["feature", "network", "incoming"], incoming);
    check_filter(
        "outgoing",
        outgoing
            .as_ref()
            .and_then(|outgoing| outgoing.filter.as_ref()),
    )?;
    set(&mut config, &["feature", "network", "outgoing"], outgoing);
    check_filter("dns", dns.as_ref().and_then(|dns| dns.filter.as_ref()))?;
    set(&mut config, &["feature", "network", "dns"], dns);
    set(&mut config, &["feature", "env"], env);
    set(&mut config, &["feature", "fs"], fs);
    if let Some(CopyTargetOptions { scale_down }) = copy_target {
        let mut copy_target = Map::from_iter([("enabled".to_owned(), true.into())]);
        set(&mut copy_target, &["scale_down"], scale_down);
        set(&mut config, &["feature", "copy_target"], Some(copy_target));
        requires_operator.push("/feature/copy_target".to_owned());
    }
    if let Some(split_queues) = split_queues {
        let mut queues = Map::new();
        for QueueSplitOptions {
            queue_id,
            queue_type,
            message_filter,
        } in split_queues
        {
            let mut queue = Map::from_iter([("queue_type".to_owned(), json!(queue_type))]);
            set(&mut queue, &["message_filter"], message_filter);
            if queues.insert(queue_id.clone(), queue.into()).is_some() {
                return Err(GenerateConfigError::DuplicateQueue(queue_id));
            }
        }
        set(&mut config, &["feature", "split_queues"], Some(queues));
        requires_operator.push("/feature/split_queues".to_owned());
    }
    set(&mut config, &["agent", "namespace"], agent_namespace);
    set(&mut config, &["kube_context"], kube_context);

    Ok((config, requires_operator))
}

fn target_value(
    TargetOptions {
        r#type,
        name,
        container,
        namespace,
    }: TargetOptions,
) -> Result<Value, GenerateConfigError> {
    for (field, value) in [("name", &name), ("container", &container)] {
        match value.as_deref() {
            Some("") => return Err(GenerateConfigError::EmptyTargetField(field)),
            Some(value) if value.contains('/') => {
                return Err(GenerateConfigError::SlashInTargetField(field));
            }
            _ => {}
        }
    }
    if namespace.as_deref() == Some("") {
        return Err(GenerateConfigError::EmptyTargetField("namespace"));
    }

    let type_name: &'static str = r#type.into();
    let path = match (r#type, name) {
        (TargetType::Targetless, None) if container.is_none() => type_name.to_owned(),
        (TargetType::Targetless, _) => return Err(GenerateConfigError::TargetlessWithName),
        (_, None) => return Err(GenerateConfigError::MissingTargetName(type_name)),
        (_, Some(name)) => match container {
            Some(container) => format!("{type_name}/{name}/container/{container}"),
            None => format!("{type_name}/{name}"),
        },
    };

    let mut target = Map::from_iter([("path".to_owned(), path.into())]);
    set(&mut target, &["namespace"], namespace);
    Ok(target.into())
}

fn check_filter(
    section: &'static str,
    filter: Option<&AddressFilter>,
) -> Result<(), GenerateConfigError> {
    match filter {
        Some(AddressFilter { remote, local }) if remote.is_some() == local.is_some() => {
            Err(GenerateConfigError::AddressFilter(section))
        }
        _ => Ok(()),
    }
}

/// Builds a `mirrord-up.yaml`, along with pointers to the options in it that need the operator.
fn up_config(
    common: Option<CommonOptions>,
    services: Vec<ServiceOptions>,
) -> Result<(Value, Vec<String>), GenerateConfigError> {
    if services.is_empty() {
        return Err(GenerateConfigError::NoServices);
    }

    let mut config = Map::new();
    if let Some(CommonOptions { kube_context }) = common {
        let mut common = Map::new();
        set(&mut common, &["context"], kube_context);
        config.insert("common".to_owned(), common.into());
    }

    let mut requires_operator = Vec::new();
    let mut up_services = Map::new();
    for ServiceOptions {
        name,
        run,
        mode,
        config,
    } in services
    {
        let config = config.unwrap_or_default();
        let incoming = config.incoming.as_ref();
        if name.is_empty() {
            return Err(GenerateConfigError::EmptyServiceName);
        }
        if run.command.is_empty() {
            return Err(GenerateConfigError::EmptyCommand(name));
        }
        if incoming.is_some_and(|incoming| incoming.mode.is_some()) {
            return Err(GenerateConfigError::IncomingModeInService(name));
        }
        if mode == Some(ServiceMode::Replace)
            && incoming.is_some_and(|incoming| incoming.http_filter.is_some())
        {
            return Err(GenerateConfigError::HttpFilterWithReplace(name));
        }

        let service_pointer = format!("/services/{}", escape_pointer_token(&name));
        let (mut patch, patch_requires_operator) = layer_config(config)?;

        let mut service = Map::new();
        for (field, layer_path) in SERVICE_FIELDS {
            if let Some(value) = take(&mut patch, layer_path) {
                service.insert((*field).to_owned(), value);
            }
        }
        requires_operator.extend(patch_requires_operator.into_iter().map(|pointer| {
            let moved = SERVICE_FIELDS
                .iter()
                .find(|(_, layer_path)| pointer == format!("/{}", layer_path.join("/")));
            match moved {
                Some((field, _)) => format!("{service_pointer}/{field}"),
                None => format!("{service_pointer}/config_patch{pointer}"),
            }
        }));
        if patch.is_empty().not() {
            service.insert("config_patch".to_owned(), patch.into());
        }
        if let Some(mode) = mode {
            if mode == ServiceMode::Replace {
                requires_operator.push(format!("{service_pointer}/default_mode"));
            }
            service.insert("default_mode".to_owned(), json!(mode));
        }
        service.insert("run".to_owned(), json!(run));

        if up_services.insert(name.clone(), service.into()).is_some() {
            return Err(GenerateConfigError::DuplicateService(name));
        }
    }
    config.insert("services".to_owned(), up_services.into());
    requires_operator.sort();

    Ok((config.into(), requires_operator))
}

/// Sets `value` at `path` in `config`, creating the objects on the way, unless it is `None`.
fn set(config: &mut Map<String, Value>, path: &[&str], value: Option<impl Serialize>) {
    let Some(value) = value else {
        return;
    };
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut object = config;
    for parent in parents {
        object = match object.entry(*parent).or_insert_with(|| Map::new().into()) {
            Value::Object(object) => object,
            _ => unreachable!("only objects are set on the way to an option"),
        };
    }
    object.insert((*last).to_owned(), json!(value));
}

/// Removes the value at `path` from `config`, along with the objects it leaves empty.
fn take(config: &mut Map<String, Value>, path: &[&str]) -> Option<Value> {
    let (first, rest) = path.split_first()?;
    if rest.is_empty() {
        return config.remove(*first);
    }
    let Value::Object(object) = config.get_mut(*first)? else {
        return None;
    };
    let value = take(object, rest);
    if object.is_empty() {
        config.remove(*first);
    }
    value
}

