use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    ops::Not,
    path::PathBuf,
    sync::Arc,
};

use clap::{ValueEnum, builder::PossibleValue};
use itertools::Itertools;
use miette::Diagnostic;
use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, ConfigError, EnvKey, MirrordConfig},
    feature::{
        copy_target::CopyTargetConfig,
        database_branches::{
            AllSplitConfigDbBranches, DatabaseBranchesConfig, SplitConfigDbBranches,
        },
        env::{EnvConfig, EnvFileConfig},
        network::incoming::{
            IncomingMode,
            http_filter::{HttpFilterConfig, HttpFilterFileConfig},
        },
        split_queues::{QueueKind, QueueMode, SplitQueuesConfig},
    },
    target::{Target, TargetType, deployment::DeploymentTarget},
};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Unexpected, Visitor, value::MapAccessDeserializer},
};
use strum::VariantArray;
use strum_macros::{Display, IntoStaticStr, VariantArray};
use thiserror::Error;

use crate::{UpError, kube_context::UpKubeContext};

/// Incoming traffic mode for a service. Also used as the mode for splitting the service's queues.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    PartialEq,
    VariantArray,
    IntoStaticStr,
    Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ServiceMode {
    /// Incoming traffic is split between the local and the original service using HTTP filtering.
    ///
    /// Uses the service's `http_filter` if set. Otherwise, requests with a header matching the
    /// regex `baggage: .*mirrord-session={key}.*` go to the local service, where `{key}` is the
    /// session key.
    ///
    /// With a workload target and no `db_branches` of its own, the service requests DB branching
    /// (`db_branches: "*"`): it runs against branches of the databases defined on the workload's
    /// `MirrordSplitConfig`, shared with the other services of the run that use the same ones.
    #[default]
    Split,

    /// The local service takes over *all* incoming traffic: the target workload is copied and the
    /// original is scaled down to zero.
    ///
    /// The service's `http_filter` is ignored, with a warning. The target must support
    /// `copy_target` with `scale_down`.
    #[schemars(extend("x-mirrord-plan" = mirrord_config::plan::Plan::Team))]
    Replace,

    /// Incoming traffic is mirrored to the local service, leaving traffic to the original service
    /// unaltered.
    ///
    /// Uses the service's `http_filter` if set. Otherwise, requests with a header matching the
    /// regex `baggage: .*mirrord-session={key}.*` are mirrored, where `{key}` is the session key.
    ///
    /// With a workload target and no `db_branches` of its own, the service requests DB branching
    /// (`db_branches: "*"`): it runs against branches of the databases defined on the workload's
    /// `MirrordSplitConfig`, shared with the other services of the run that use the same ones.
    // The fallback filter and the DB branching request are built in `ServiceConfig::assemble`.
    Mirror,
}

// Manual impl because we want different user-facing docs
impl ValueEnum for ServiceMode {
    fn value_variants<'a>() -> &'a [Self] {
        Self::VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        let name: &'static str = self.into();
        match self {
            ServiceMode::Split => Some(PossibleValue::new(name).help("Incoming traffic is split between the local and original services using HTTP filtering.
If no filter is provided, a header filter matching the regex `baggage: .*mirrord-session={session key}.*` will be used.")),
            ServiceMode::Replace => Some(PossibleValue::new(name).help("The local service replaces the original one: the target workload is copied and scaled down to zero, so all incoming traffic reaches your local process.
Any configured HTTP filter is ignored. Requires a target that supports copy target with scale down.")),
            ServiceMode::Mirror => Some(PossibleValue::new(name).help("Incoming traffic is mirrored to the local service using HTTP filtering.
If no filter is provided, a header filter matching the regex `baggage: .*mirrord-session={session key}.*` will be used.")),
        }
    }
}

#[allow(clippy::from_over_into)]
impl Into<IncomingMode> for ServiceMode {
    fn into(self) -> IncomingMode {
        match self {
            ServiceMode::Split | ServiceMode::Replace => IncomingMode::Steal,
            ServiceMode::Mirror => IncomingMode::Mirror,
        }
    }
}

#[allow(clippy::from_over_into)]
impl Into<QueueMode> for ServiceMode {
    fn into(self) -> QueueMode {
        match self {
            ServiceMode::Split | ServiceMode::Replace => QueueMode::Steal,
            ServiceMode::Mirror => QueueMode::Mirror,
        }
    }
}

/// Whether the service runs with `mirrord exec` or `mirrord container`.
#[derive(
    Clone,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    PartialEq,
    Default,
    IntoStaticStr,
    Display,
    VariantArray,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum RunType {
    /// Run `command` as a local process with `mirrord exec`.
    #[default]
    Exec,

    /// Run `command` with `mirrord container`. The command must start a container, e.g.
    /// `["docker", "run", "my-image"]`.
    Container,
}

/// Includes all information necessary to build the `mirrord` command
/// for this session.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub struct RunConfig {
    /// Whether to run `command` with `mirrord exec` or `mirrord container`.
    ///
    /// Defaults to `exec`.
    #[serde(default)]
    pub r#type: RunType,

    /// Working directory for `command`. Relative paths are resolved against the directory that
    /// contains `mirrord-up.yaml`, and the directory must exist.
    ///
    /// Only supported with `type` `exec`. When not set, `command` runs in the directory
    /// `mirrord up` was started from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,

    /// The command to run, as a list of the program and its arguments, e.g.
    /// `["npm", "run", "dev"]`.
    pub command: Vec<String>,

    /// A command to run to completion before the service's mirrord session starts, as a list of
    /// the program and its arguments, e.g. `["npm", "run", "build"]`.
    ///
    /// Runs locally without mirrord, in the same directory as `command`. The session starts
    /// only if it exits successfully; if it fails, this service is not started and the other
    /// services keep running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_command: Vec<String>,
}

/// Settings applied to every service.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct CommonConfig {
    /// Accept invalid TLS certificates (e.g. self-signed) from the Kubernetes API server. When not
    /// set, the value from the kubeconfig is used.
    pub(crate) accept_invalid_certificates: Option<bool>,

    /// Whether to use the mirrord operator. When not set, mirrord tries the operator and continues
    /// without it if that fails.
    pub(crate) operator: Option<bool>,

    /// Whether mirrord sends telemetry data to MetalBear cloud.
    pub(crate) telemetry: Option<bool>,

    /// Kube context to use for all services, from your kubeconfig.
    ///
    /// A service's own `context` takes precedence, and `mirrord up --context` takes precedence
    /// over both. When none is set, the global config's `kube_context` is used, then the
    /// kubeconfig's current context.
    pub(crate) context: Option<Arc<str>>,
}

impl CommonConfig {
    /// The mirrord config every service is assembled from: these settings plus whatever
    /// `context` takes from the environment.
    fn base_config(&self, context: &mut ConfigContext) -> Result<LayerConfig, ConfigError> {
        LayerFileConfig {
            accept_invalid_certificates: self.accept_invalid_certificates,
            operator: self.operator,
            telemetry: self.telemetry,
            ..Default::default()
        }
        .generate_config(context)
    }
}

/// The target workload of a service.
// Separate from [`mirrord_config::target::TargetConfig`] because we use custom serde attributes
// for this that we don't want on the other one.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct SpecifiedTarget {
    /// The workload to target, e.g. `deployment/my-app` or `pod/my-pod/container/my-container`.
    ///
    /// When not set, `mirrord up` looks for a deployment, statefulset, rollout or pod (in that
    /// order) named after the service.
    #[serde(
        default,
        deserialize_with = "mirrord_config::util::string_or_struct_option",
        serialize_with = "serialize_path",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "mirrord_config::target::make_simple_target_custom_schema")]
    pub(crate) path: Option<Target>,

    /// Namespace of the target workload. Defaults to the namespace of the kube context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) namespace: Option<Arc<str>>,
}

/// The three states a service's `target` field can be in.
#[derive(Clone, Debug, PartialEq)]
pub enum TargetConfig {
    /// `target: none`
    Targetless,

    /// Traditional `target: { path: "pod/path", namespace: "some_namespace" }` syntax
    Specified(SpecifiedTarget),
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self::UNSPECIFIED
    }
}

// Hand-written to match the hand-written `Deserialize` impl below.
impl JsonSchema for TargetConfig {
    fn schema_name() -> Cow<'static, str> {
        "TargetConfig".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [
                {
                    "description": "Run targetless.",
                    "const": "none"
                },
                generator.subschema_for::<SpecifiedTarget>(),
                {
                    "description": "An empty `target`, treated as if it were not set.",
                    "type": "null"
                }
            ]
        })
    }
}

impl From<SpecifiedTarget> for TargetConfig {
    fn from(value: SpecifiedTarget) -> Self {
        Self::Specified(value)
    }
}

impl<'de> Deserialize<'de> for TargetConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TargetVisitor;
        const EXPECTED: &str = "`none`, a target mapping, or nothing at all";

        impl<'de> Visitor<'de> for TargetVisitor {
            type Value = TargetConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(EXPECTED)
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                SpecifiedTarget::deserialize(MapAccessDeserializer::new(map))
                    .map(TargetConfig::Specified)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if v == "none" {
                    return Ok(TargetConfig::Targetless);
                }

                // `invalid_value` has no list of allowed names for the parse error to suggest
                // from, so the hint goes in the expected text.
                let expected: Cow<str> = match crate::closest_name(v, &["none"]) {
                    Some(closest) => format!("{EXPECTED}; did you mean `{closest}`?").into(),
                    None => EXPECTED.into(),
                };
                Err(serde::de::Error::invalid_value(
                    Unexpected::Str(v),
                    &expected.as_ref(),
                ))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TargetConfig::UNSPECIFIED)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                deserializer.deserialize_any(self)
            }
        }

        deserializer.deserialize_option(TargetVisitor)
    }
}

impl TargetConfig {
    pub const UNSPECIFIED: Self = Self::Specified(SpecifiedTarget {
        path: None,
        namespace: None,
    });

    #[cfg(test)]
    fn unwrap_specified(&self) -> &SpecifiedTarget {
        match self {
            TargetConfig::Specified(specified_target) => specified_target,
            _ => panic!("did not parse as TargetConfig::Specified, parsed as {self:?}"),
        }
    }

    /// Return a [`mirrord_config::target::TargetConfig`] when we have a path or are explicitly
    /// targetless, OR an [`UnresolvedTarget`] otherwise, when we need to resolve the target
    /// workload by querying the cluster against `name` using `kube_context`.
    fn as_resolved(
        &self,
        name: &Arc<str>,
        kube_context: Option<Arc<str>>,
    ) -> Result<mirrord_config::target::TargetConfig, UnresolvedTarget> {
        match self {
            Self::Targetless => Ok(mirrord_config::target::TargetConfig {
                path: None,
                namespace: None,
            }),

            Self::Specified(SpecifiedTarget {
                path: None,
                namespace,
            }) => Err(UnresolvedTarget {
                workload_name: Arc::clone(name),
                namespace: namespace.clone(),
                kube_context,
            }),

            Self::Specified(SpecifiedTarget {
                path: path @ Some(_),
                namespace,
            }) => Ok(mirrord_config::target::TargetConfig {
                path: path.clone(),
                namespace: namespace.as_deref().map(Into::into),
            }),
        }
    }
}

impl Serialize for TargetConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            TargetConfig::Targetless => serializer.serialize_str("none"),
            TargetConfig::Specified(specified) => specified.serialize(serializer),
        }
    }
}

fn serialize_path<S>(path: &Option<Target>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match path {
        Some(target) => serializer.serialize_str(&target.to_string()),
        None => serializer.serialize_none(),
    }
}

/// Inlines `T`'s schema without its `description` and its properties' `title`s.
///
/// Used for mirrord-config types whose docs are written for `mirrord.json` and don't apply to
/// `mirrord-up.yaml`. The field's own doc comment becomes the description instead.
fn inline_schema_without_docs<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    let mut schema = T::json_schema(generator);
    schema.remove("description");

    if let Some(serde_json::Value::Object(properties)) = schema.get_mut("properties") {
        for property in properties
            .values_mut()
            .filter_map(serde_json::Value::as_object_mut)
        {
            property.remove("title");
        }
    }

    schema
}

/// Per-service configuration.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// The remote workload this service runs against.
    ///
    /// - `none`: run targetless.
    /// - A mapping with `path` and/or `namespace`: run against that workload.
    /// - Not set: `mirrord up` looks for a deployment, statefulset, rollout or pod (in that order)
    ///   named after the service.
    #[serde(default)]
    pub(crate) target: TargetConfig,

    /// Environment variable settings for the local process.
    ///
    /// For example:
    ///
    /// ```yaml
    /// env:
    ///   include: "DATABASE_USER;PUBLIC_*"
    ///   override:
    ///     LOG_LEVEL: debug
    /// ```
    #[serde(default)]
    #[schemars(schema_with = "inline_schema_without_docs::<EnvFileConfig>")]
    pub(crate) env: EnvConfig,

    /// How incoming traffic and queue messages are shared between the local and the original
    /// service. Defaults to `split`.
    ///
    /// `mirrord up --mode` overrides this for every service.
    #[serde(default)]
    pub(crate) default_mode: ServiceMode,

    /// HTTP filter that selects which requests reach the local service in `split` and `mirror`
    /// modes.
    ///
    /// When not set, requests with a header matching the regex
    /// `baggage: .*mirrord-session={key}.*` are selected, where `{key}` is the session key.
    /// Ignored in `replace` mode. For example:
    ///
    /// ```yaml
    /// http_filter:
    ///   header_filter: "^x-debug: true$"
    /// ```
    #[serde(default)]
    #[schemars(schema_with = "inline_schema_without_docs::<HttpFilterFileConfig>")]
    pub(crate) http_filter: HttpFilterConfig,

    /// Ports whose incoming traffic is not stolen or mirrored.
    #[serde(default)]
    pub(crate) ignore_ports: BTreeSet<u16>,

    /// Don't launch this service unless it is named on the `mirrord up` command line. Defaults to
    /// `false`.
    #[serde(default)]
    pub(crate) skip: bool,

    /// Kube context to use for this service, from your kubeconfig.
    ///
    /// Takes precedence over `common.context`. `mirrord up --context` takes precedence over both.
    #[serde(default)]
    pub(crate) context: Option<Arc<str>>,

    /// A JSON merge patch (RFC 7396) applied to the mirrord config generated for this service.
    ///
    /// Not to be relied upon, `mirrord up` tries to do the right thing by default, so you should
    /// only reach for this when truly needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) config_patch: Option<serde_json::Value>,

    /// Container image to run in this service's preview environment, as a full image ref, e.g.
    /// `ghcr.io/acme/api:3f9a2c1`.
    ///
    /// Used only by `mirrord preview up`, which starts a preview for every service that has an
    /// image. `mirrord preview up --image <service>=<ref>` and `MIRRORD_<SERVICE>_IMAGE` take
    /// precedence over it. `mirrord up` ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-mirrord-plan" = mirrord_config::plan::Plan::Enterprise))]
    pub(crate) image: Option<String>,

    /// How to launch the service's local process.
    pub(crate) run: RunConfig,
}

/// Top-level `mirrord-up.yaml` configuration.
///
/// The file is rendered as a template before parsing: `{{ key }}` inserts the session key, and
/// `{{ git_branch }}` inserts the current git branch when there is one.
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct UpConfig {
    /// Settings applied to all services.
    #[serde(default)]
    #[schemars(extend("x-mirrord-plan" = mirrord_config::plan::Plan::Oss))]
    pub common: CommonConfig,
    /// Per-service configurations keyed by service name.
    #[schemars(extend("x-mirrord-plan" = mirrord_config::plan::Plan::Oss))]
    pub services: HashMap<Arc<str>, ServiceConfig>,
}

/// A selected `mirrord up` configuration that would delegate to commands unsupported on Windows.
#[derive(Debug, Error, Diagnostic)]
pub enum WindowsSupportError {
    /// A service uses `mirrord container`.
    #[error("`mirrord container` is not supported on Windows for service: {0}")]
    Container(Arc<str>),

    /// One or more exec services would be rerouted through mirrord for CI.
    #[error("mirrord for CI is not supported on Windows")]
    Ci,
}

/// The settings of `common` in a `mirrord-up.yaml` that end up in the mirrord config generated
/// for each service, with the `mirrord.json` option each one sets. Their own settings map onto the
/// same names below that option. `context` is applied through the kube context `mirrord up`
/// picks, the others by [`ServiceConfig::assemble`], which this must be changed along with.
pub const COMMON_LAYER_PATHS: &[(&str, &str)] = &[
    ("accept_invalid_certificates", "accept_invalid_certificates"),
    ("operator", "operator"),
    ("telemetry", "telemetry"),
    ("context", "kube_context"),
];

/// The settings of a service in a `mirrord-up.yaml` that [`ServiceConfig::assemble`] copies into
/// the mirrord config it generates for the service, with the `mirrord.json` option each one sets,
/// and must be changed along with it. Their own settings map onto the same names below that
/// option (e.g. `http_filter.header_filter`); a service's `config_patch` is a mirrord config
/// itself. Settings missing here, like `run`, only steer `mirrord up`.
pub const SERVICE_LAYER_PATHS: &[(&str, &str)] = &[
    ("context", "kube_context"),
    ("target", "target"),
    ("env", "feature.env"),
    ("default_mode", "feature.network.incoming.mode"),
    ("http_filter", "feature.network.incoming.http_filter"),
    ("ignore_ports", "feature.network.incoming.ignore_ports"),
];

impl ServiceConfig {
    /// Build a ([`LayerConfig`], [`RunConfig`]) pair for this service, on top of `base` from
    /// [`CommonConfig::base_config`].
    ///
    /// For [`SessionKind::Preview`], the config is for the service's `mirrord preview up` session
    /// instead: it is assembled as in `split` mode whatever the service's `default_mode`, without
    /// `env`, and runs the service's `image`.
    fn assemble(
        mut self,
        service_name: &Arc<str>,
        base: LayerConfig,
        key: EnvKey,
        resolved_targets: &mut HashMap<UnresolvedTarget, ResolvedTarget>,
        up_context: UpKubeContext,
        kind: SessionKind,
    ) -> Result<SubprocessCfg, ConfigPatchErrorKind> {
        let preview = kind == SessionKind::Preview;
        if preview {
            self.default_mode = ServiceMode::Split;
            self.env = EnvConfig::default();
        }

        let kube_context = up_context.get_context(self.context.clone());
        let mut cfg = self.unpatched_config(base, &key, kube_context.clone());

        // `PreviewIncomingConfig` falls back to the anchored `^baggage: ...$` filter only when
        // none is set, and share ingress only serves sessions with exactly that filter, so up's
        // own default must not get in the way.
        if preview && self.http_filter.is_filter_set().not() {
            cfg.feature.network.incoming.http_filter = HttpFilterConfig::default();
        }

        cfg.target = match self.target.as_resolved(service_name, kube_context) {
            Ok(resolved) => resolved,
            Err(unresolved) => match resolved_targets.remove(&unresolved) {
                Some(target) => {
                    assert!(
                        target.resolved.path.is_some(),
                        "resolved workload must have a non-null path"
                    );
                    mirrord_config::target::TargetConfig {
                        path: target.resolved.path,
                        namespace: target.resolved.namespace.as_deref().map(Into::into),
                    }
                }
                None => panic!(
                    "No resolved entry for `{}` in `resolved_targets`",
                    unresolved.workload_name
                ),
            },
        };

        // The operator resolves `"*"` against the `MirrordSplitConfig` of one targeted
        // workload, so a targetless or label-selected service has nothing to ask for. Every
        // service of a run shares the key, so services pointing at one entry land on one branch.
        // Branches set in the base config, or by `config_patch` below, take precedence.
        let single_workload = cfg.target.path.as_ref().is_some_and(|path| {
            matches!(
                path,
                Target::Targetless | Target::Serverless(_) | Target::Label(_)
            )
            .not()
        });
        let mut default_db_branches =
            matches!(self.default_mode, ServiceMode::Split | ServiceMode::Mirror)
                && single_workload
                && cfg.feature.db_branches.is_empty();
        if default_db_branches {
            cfg.feature.db_branches = DatabaseBranchesConfig::FromSplitConfig(
                SplitConfigDbBranches::All(AllSplitConfigDbBranches::All),
            );
        }

        // A preview session that asks for any queue split is refused by the operator unless its
        // target is a workload it can look a `MirrordSplitConfig` up for, even when `"*"` would
        // match no queue. `mirrord up` sessions skip such targets instead, so leaving the splits
        // out ends the same way.
        if preview
            && matches!(
                cfg.target.path,
                Some(Target::Deployment(_) | Target::Rollout(_) | Target::StatefulSet(_))
            )
            .not()
        {
            cfg.feature.split_queues = SplitQueuesConfig::default();
        }
        let mut default_split_queues = cfg.feature.split_queues.is_set();

        if let Some(patch) = self.config_patch {
            if preview && patch.pointer("/feature/preview/image").is_some() {
                return Err(ConfigPatchErrorKind::PreviewImage);
            }

            // A patch that sets `db_branches` or `split_queues` decides them itself, `[]`
            // included, so whatever it holds afterwards was asked for by the user.
            default_db_branches &= patch.pointer("/feature/db_branches").is_none();
            default_split_queues &= patch.pointer("/feature/split_queues").is_none();
            cfg = apply_config_patch(cfg, patch, &key)?;
        }

        if preview {
            cfg.feature.preview.image = self.image;
        }

        Ok(SubprocessCfg {
            config: cfg,
            service_name: service_name.clone(),
            run: self.run,
            mode: self.default_mode,
            default_db_branches,
            default_split_queues,
        })
    }

    /// The mirrord config `mirrord up` merges this service's `config_patch` into, as JSON, but
    /// without the target, which needs the cluster to resolve. Lets a patch be checked against
    /// what it is merged into without running it (e.g. in `mirrord mcp`), where an issue can be
    /// told apart as the patch's or a setting's. Takes the service on its own, so the rest of a
    /// `mirrord-up.yaml` doesn't have to be valid.
    ///
    /// Like [`UpConfig::verify_services`], ignores `MIRRORD_*` environment variables.
    pub fn unpatched_config_json(
        &self,
        service_name: &Arc<str>,
        common: &CommonConfig,
        key: &EnvKey,
    ) -> Result<serde_json::Value, UpError> {
        let base = common.base_config(&mut ConfigContext::default().strict_env(true))?;
        let kube_context = UpKubeContext {
            command_arg: None,
            common_context: common.context.clone(),
        }
        .get_context(self.context.clone());
        let config = self.unpatched_config(base, key, kube_context);
        patchable(config, key).map_err(|source| {
            ConfigPatchError {
                service: service_name.clone(),
                source: ConfigPatchErrorKind::Serialize(source),
            }
            .into()
        })
    }

    /// The mirrord config of this service before its target is set and its `config_patch` is
    /// merged in: `base` with the service's own settings and what its mode implies.
    fn unpatched_config(
        &self,
        mut cfg: LayerConfig,
        key: &EnvKey,
        kube_context: Option<Arc<str>>,
    ) -> LayerConfig {
        cfg.kube_context = kube_context.as_deref().map(Into::into);
        cfg.feature.env = self.env.clone();
        cfg.feature.network.incoming.mode = self.default_mode.into();

        match self.default_mode {
            ServiceMode::Split | ServiceMode::Mirror => {
                cfg.feature.network.incoming.http_filter = if self.http_filter.is_filter_set() {
                    self.http_filter.clone()
                } else {
                    HttpFilterConfig {
                        header_filter: Some(format!(
                            "baggage: .*mirrord-session={}.*",
                            key.as_str()
                        )),
                        ..Default::default()
                    }
                };

                cfg.feature.split_queues =
                    SplitQueuesConfig::all_wildcard_with_mode(key, self.default_mode.into());
            }

            // `Replace` mode should steal all traffic from the copied target, so `http_filter` is
            // ignored; `service_configs` tells the user.
            ServiceMode::Replace => {
                cfg.feature.copy_target = CopyTargetConfig {
                    enabled: true,
                    scale_down: true,
                    exclude_containers: Vec::new(),
                    exclude_init_containers: Vec::new(),
                };
            }
        }

        cfg.feature.network.incoming.ignore_ports = self.ignore_ports.iter().copied().collect();
        cfg.key = key.clone();
        cfg
    }

    fn resolve_target(
        &self,
        name: &Arc<str>,
        up_context: UpKubeContext,
    ) -> Result<mirrord_config::target::TargetConfig, UnresolvedTarget> {
        self.target
            .as_resolved(name, up_context.get_context(self.context.clone()))
    }
}

/// Error produced when a service's `config_patch` cannot produce a valid mirrord config.
#[derive(Debug, Error, Diagnostic)]
#[error("invalid `config_patch` for service `{service}`: {source}")]
pub struct ConfigPatchError {
    service: Arc<str>,
    #[source]
    source: ConfigPatchErrorKind,
}

#[derive(Debug, Error)]
enum ConfigPatchErrorKind {
    #[error("failed to serialize the generated mirrord config: {0}")]
    Serialize(serde_json::Error),

    #[error("the merged result does not match the mirrord config schema: {0}")]
    Schema(serde_json::Error),

    #[error("the merged mirrord config failed validation: {0}")]
    Validation(#[from] mirrord_config::config::ConfigError),

    #[error(
        "it sets `feature.preview.image`, which `mirrord preview up` takes from the service's \
         `image`, `--image` or `MIRRORD_<SERVICE>_IMAGE` instead"
    )]
    PreviewImage,
}

/// Here we merge the `config` with the `patch` (`config_patch` from the `mirrord-up.yaml` config
/// file) for the service.
///
/// ```text
/// mirrord-up.yaml
///     │
///     ├─ Tera renders the entire YAML
///     │    {{ key }}, {{ git_branch }}, get_env(...)
///     │
///     ├─ YAML is deserialized into UpConfig
///     │
///     └─ For each selected service
///          │
///          ├─ Generate the normal mirrord-up LayerConfig
///          ├─ Apply that service's config_patch
///          ├─ Validate the merged config
///          └─ Encode the final LayerConfig
/// ```
fn apply_config_patch(
    config: LayerConfig,
    patch: serde_json::Value,
    key: &EnvKey,
) -> Result<LayerConfig, ConfigPatchErrorKind> {
    let generated_kafka_splits = config
        .feature
        .split_queues
        .of_kind(QueueKind::Kafka)
        .cloned()
        .collect::<Vec<_>>();

    let mut layer_config_json = patchable(config, key).map_err(ConfigPatchErrorKind::Serialize)?;
    json_patch::merge(&mut layer_config_json, &patch);

    let file_config: LayerFileConfig =
        serde_json::from_value(layer_config_json).map_err(ConfigPatchErrorKind::Schema)?;

    // The generated config already contains all environment-derived settings. Isolating this
    // second generation pass keeps environment variables from unexpectedly overriding the patch.
    let mut context = ConfigContext::default().strict_env(true);
    let mut config = file_config.generate_config(&mut context)?;
    config.key = key.clone();

    // A Kafka branch serves every Kafka queue, so the Kafka splits the mode generated give way.
    if config.feature.kafka_branch {
        config.feature.split_queues = SplitQueuesConfig::from_splits(
            config
                .feature
                .split_queues
                .splits()
                .iter()
                .filter(|split| generated_kafka_splits.contains(split).not())
                .cloned(),
        );
    }

    config.verify(&mut context)?;

    Ok(config)
}

/// `config` as the JSON a `config_patch` is merged into.
fn patchable(config: LayerConfig, key: &EnvKey) -> Result<serde_json::Value, serde_json::Error> {
    let mut json = serde_json::to_value(config)?;

    // `LayerConfig` preserves whether a key was provided or generated in its serialized form,
    // while a user-facing mirrord config represents it as a string.
    if let Some(object) = json.as_object_mut() {
        object.insert(
            "key".to_owned(),
            serde_json::Value::String(key.as_str().to_owned()),
        );
    }

    Ok(json)
}

/// Which command a service's session is assembled for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionKind {
    /// A `mirrord up` session, running the service's local process.
    Up,
    /// A `mirrord preview up` session, running the service's `image` in the cluster.
    Preview,
}

/// All the information necessary to start and manage one of the child
/// mirrord sessions.
pub struct SubprocessCfg {
    /// The resolved layer configuration for this child process.
    pub config: LayerConfig,
    /// Name of the service this subprocess runs.
    pub service_name: Arc<str>,
    /// How to run this service (exec vs container, and the command).
    pub run: RunConfig,
    /// Mode this service was assembled for. Its effects are already in `config`; it names the
    /// mode in [`validate_targets`] errors and in what `mirrord up` reports about the service.
    pub mode: ServiceMode,
    /// `config.feature.db_branches` is the `"*"` `mirrord up` adds for this service's mode, not
    /// one the user set. An operator that can't resolve it drops it rather than failing the run.
    pub default_db_branches: bool,
    /// `config.feature.split_queues` are the `"*"` splits `mirrord up` adds for this service's
    /// mode, not ones the user set. A preview session asks its operator to drop the ones it
    /// can't serve rather than failing.
    pub default_split_queues: bool,
}

/// Rejects services whose resolved target cannot support the features their
/// mode enabled.
pub(crate) fn validate_targets(cfgs: &[SubprocessCfg]) -> Result<(), ModeError> {
    let incompatible = cfgs
        .iter()
        .filter_map(|cfg| {
            let target_type = cfg
                .config
                .target
                .path
                .as_ref()
                .map_or(TargetType::Targetless, TargetType::from);

            target_type
                .compatible_with(&cfg.config.feature)
                .not()
                .then(|| IncompatibleTarget {
                    service: cfg.service_name.clone(),
                    mode: cfg.mode,
                    target_type,
                })
        })
        .collect::<Vec<_>>();

    if incompatible.is_empty().not() {
        return Err(ModeError::IncompatibleTargets(incompatible));
    }

    Ok(())
}

/// A service whose mode and target cannot be used together.
#[derive(Debug)]
pub struct IncompatibleTarget {
    /// Name of the offending service.
    pub service: Arc<str>,
    /// Mode the service was configured to run in.
    pub mode: ServiceMode,
    /// Kind of the target the service resolved to.
    pub target_type: TargetType,
}

impl std::fmt::Display for IncompatibleTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            service,
            mode,
            target_type,
        } = self;

        write!(
            f,
            "`{service}` is `{mode}` mode with a {target_type} target"
        )
    }
}

/// Error produced by [`validate_targets`].
#[derive(Debug, Error, Diagnostic)]
pub enum ModeError {
    /// One or more services resolved to a target their mode cannot use.
    #[error("incompatible mode and target: {}", .0.iter().format("; "))]
    IncompatibleTargets(Vec<IncompatibleTarget>),
}

/// A service that `mirrord up` would refuse to run, found by [`UpConfig::verify_services`].
#[derive(Debug, Error)]
pub enum ServiceError {
    /// The service's `config_patch` doesn't merge into a valid config.
    #[error(transparent)]
    ConfigPatch(#[from] ConfigPatchError),

    /// The service's assembled config fails the checks its `mirrord exec` session runs at
    /// startup.
    #[error("invalid mirrord config for service `{service}`: {source}")]
    Validation {
        /// Name of the offending service.
        service: Arc<str>,
        /// What the checks rejected.
        #[source]
        source: ConfigError,
    },

    /// The service's mode needs features its target doesn't support.
    #[error("incompatible mode and target: {0}")]
    IncompatibleTarget(IncompatibleTarget),
}

impl ServiceError {
    /// The name of the offending service.
    pub fn service(&self) -> &Arc<str> {
        match self {
            Self::ConfigPatch(error) => &error.service,
            Self::Validation { service, .. } => service,
            Self::IncompatibleTarget(target) => &target.service,
        }
    }
}

impl UpConfig {
    /// Uses `context` as the common context when the config sets none.
    pub fn default_context(&mut self, context: Option<Arc<str>>) {
        if self.common.context.is_none() {
            self.common.context = context;
        }
    }

    /// Verifies the config without touching the cluster, similar to
    /// what `verify_config` does for [`LayerConfig`].
    pub fn verify_services(&self, key: &EnvKey) -> Result<Vec<ServiceError>, ConfigError> {
        let base = self
            .common
            .base_config(&mut ConfigContext::default().strict_env(true))?;
        let up_context = UpKubeContext {
            command_arg: None,
            common_context: self.common.context.clone(),
        };
        let mut resolved_targets = self
            .unresolved_targets(up_context.clone())
            .map(|unresolved| {
                let resolved = ResolvedTarget {
                    resolved: SpecifiedTarget {
                        path: Some(Target::Deployment(DeploymentTarget {
                            deployment: unresolved.workload_name.to_string(),
                            container: None,
                        })),
                        namespace: unresolved.namespace.clone(),
                    },
                };
                (unresolved, resolved)
            })
            .collect();

        let mut errors = Vec::new();
        let mut assembled = Vec::new();
        for (service_name, service) in self.services.iter().sorted_by_key(|(name, _)| *name) {
            let subprocess = match service.clone().assemble(
                service_name,
                base.clone(),
                key.clone(),
                &mut resolved_targets,
                up_context.clone(),
                SessionKind::Up,
            ) {
                Ok(assembled) => assembled,
                Err(source) => {
                    errors.push(ServiceError::ConfigPatch(ConfigPatchError {
                        service: service_name.clone(),
                        source,
                    }));
                    continue;
                }
            };

            // As in the service's `mirrord exec` session, which doesn't take a missing target for
            // targetless when verifying; `validate_targets` checks the mode against it below.
            let mut context = ConfigContext::default().strict_env(true);
            if let Err(source) = subprocess.config.verify(&mut context) {
                errors.push(ServiceError::Validation {
                    service: service_name.clone(),
                    source,
                });
                continue;
            }

            assembled.push(subprocess);
        }

        if let Err(ModeError::IncompatibleTargets(incompatible)) = validate_targets(&assembled) {
            errors.extend(
                incompatible
                    .into_iter()
                    .map(ServiceError::IncompatibleTarget),
            );
        }

        Ok(errors)
    }

    /// True unless the user opted out of telemetry.
    pub fn telemetry_enabled(&self) -> bool {
        self.common.telemetry.unwrap_or(true)
    }

    /// When running `mirrord up` on Windows, validates that the up config doesn't have anything
    /// that is unsupported, for example a service that's configured to run with `mirrord
    /// container`, or a service that's configured to run with `mirrord exec` for CI.
    pub fn validate_windows(&self, ci_key_present: bool) -> Result<(), WindowsSupportError> {
        self.services
            .iter()
            .find_map(|(name, service)| match &service.run.r#type {
                RunType::Container => Some(WindowsSupportError::Container(Arc::clone(name))),
                RunType::Exec if ci_key_present => Some(WindowsSupportError::Ci),
                RunType::Exec => None,
            })
            .map_or(Ok(()), Err)
    }

    /// Force services to run in `mode`, discarding the `default_mode` set in the config file.
    ///
    /// Meant for the `--mode` flag, so it should be called *after* [`Self::select_services`].
    pub fn override_mode(&mut self, mode: ServiceMode) {
        for svc in self.services.values_mut() {
            svc.default_mode = mode;
        }
    }

    /// Restrict which services will be launched.
    ///
    /// When `selected` is non-empty, keeps exactly those services,
    /// erroring on any name that isn't in the config. Explicitly naming
    /// a service overrides its `skip` flag. When `selected` is empty,
    /// keeps every service except those marked `skip: true`.
    ///
    /// Errors if no service remains to be launched.
    pub fn select_services(&mut self, selected: &[String]) -> Result<(), SelectError> {
        if selected.is_empty() {
            self.services.retain(|_, svc| svc.skip.not());

            if self.services.is_empty() {
                return Err(SelectError::AllSkipped);
            }
        } else {
            let unknown: Vec<String> = selected
                .iter()
                .filter(|name| self.services.contains_key(name.as_str()).not())
                .cloned()
                .collect();
            if unknown.is_empty().not() {
                let mut available: Vec<String> =
                    self.services.keys().map(|name| name.to_string()).collect();
                available.sort();
                return Err(SelectError::UnknownServices { unknown, available });
            }

            self.services
                .retain(|name, _| selected.iter().any(|s| s.as_str() == name.as_ref()));
        }

        Ok(())
    }

    /// Picks the image of every service for `mirrord preview up`, writes it into the service's
    /// `image`, and drops the services that have none: those get no preview.
    ///
    /// `flags` are the `--image <service>=<ref>` values. A service's image comes from its flag,
    /// then its `MIRRORD_<SERVICE>_IMAGE` environment variable (see [`preview_image_env`]; empty
    /// counts as unset), then its `image`. `skip` doesn't apply: the images pick the services.
    /// The variables are read through `context`.
    pub fn select_preview_images(
        &mut self,
        flags: &[String],
        context: &ConfigContext,
    ) -> Result<PreviewImages, PreviewImageError> {
        let mut from_flags: HashMap<&str, &str> = HashMap::new();
        for flag in flags {
            let (service, image) = flag
                .split_once('=')
                .filter(|(service, image)| service.is_empty().not() && image.is_empty().not())
                .ok_or_else(|| PreviewImageError::InvalidFlag(flag.clone()))?;
            if self.services.contains_key(service).not() {
                return Err(PreviewImageError::UnknownService {
                    service: service.to_owned(),
                    available: self
                        .services
                        .keys()
                        .map(|name| name.to_string())
                        .sorted()
                        .collect(),
                });
            }
            if from_flags.insert(service, image).is_some() {
                return Err(PreviewImageError::DuplicateFlag(service.to_owned()));
            }
        }

        // A service with a flag doesn't read its variable, so it can't make one ambiguous.
        let mut env_names: HashMap<String, Vec<&Arc<str>>> = HashMap::new();
        for service in self
            .services
            .keys()
            .filter(|service| from_flags.contains_key(service.as_ref()).not())
        {
            env_names
                .entry(preview_image_env(service))
                .or_default()
                .push(service);
        }
        let mut from_env = HashMap::new();
        for (env, services) in env_names {
            let Some(image) = context
                .get_env(&env)
                .ok()
                .filter(|image| image.is_empty().not())
            else {
                continue;
            };
            if let [service] = services.as_slice() {
                from_env.insert((*service).clone(), image);
            } else {
                return Err(PreviewImageError::AmbiguousEnv {
                    env,
                    services: services
                        .iter()
                        .map(|name| name.to_string())
                        .sorted()
                        .collect(),
                });
            }
        }

        let mut images = PreviewImages::default();
        self.services.retain(|name, service| {
            let picked = match from_flags.get(name.as_ref()) {
                Some(image) => Some(((*image).to_owned(), PreviewImageSource::Flag)),
                None => match from_env.remove(name) {
                    Some(image) => Some((image, PreviewImageSource::Env)),
                    None => service
                        .image
                        .take()
                        .map(|image| (image, PreviewImageSource::File)),
                },
            };

            match picked {
                Some((image, source)) => {
                    service.image = Some(image);
                    images.sources.insert(name.clone(), source);
                    true
                }
                None => {
                    images.skipped.push(name.clone());
                    false
                }
            }
        });
        images.skipped.sort();

        if self.services.is_empty() {
            return Err(PreviewImageError::NoImages);
        }

        Ok(images)
    }

    /// Produces one [`SubprocessCfg`] per service defined in the configuration.
    ///
    /// `resolved_targets` must be a [`HashMap`] with an entry for
    /// every item yielded by the iterator returned from
    /// [`Self::unresolved_targets`] -- this method will panic
    /// otherwise. Entries that are used to resolve a target will be
    /// removed from the `resolved_targets`.
    ///
    /// `context` is what the configs are generated with, which reads `MIRRORD_*` environment
    /// variables unless it's [`ConfigContext::strict_env`].
    pub(crate) fn service_configs(
        self,
        key: &EnvKey,
        resolved_targets: &mut HashMap<UnresolvedTarget, ResolvedTarget>,
        up_context: UpKubeContext,
        context: &mut ConfigContext,
    ) -> Result<Vec<SubprocessCfg>, UpError> {
        self.assemble_services(key, resolved_targets, up_context, context, SessionKind::Up)
    }

    /// Like [`Self::service_configs`], for the sessions of `kind` (see
    /// [`ServiceConfig::assemble`]).
    pub(crate) fn assemble_services(
        self,
        key: &EnvKey,
        resolved_targets: &mut HashMap<UnresolvedTarget, ResolvedTarget>,
        up_context: UpKubeContext,
        context: &mut ConfigContext,
        kind: SessionKind,
    ) -> Result<Vec<SubprocessCfg>, UpError> {
        let base_config = self.common.base_config(context)?;

        self.services
            .into_iter()
            .map(|(service_name, svc)| {
                let mode = svc.default_mode;
                if kind == SessionKind::Up
                    && mode == ServiceMode::Replace
                    && svc.http_filter.is_filter_set()
                {
                    eprintln!(
                        "{service_name}: `http_filter` is ignored in `replace` mode, all incoming traffic is handled locally"
                    );
                }
                svc.assemble(
                    &service_name,
                    base_config.clone(),
                    key.clone(),
                    resolved_targets,
                    up_context.clone(),
                    kind,
                )
                .map_err(|source| {
                    ConfigPatchError {
                        service: service_name.clone(),
                        source,
                    }
                    .into()
                })
            })
            .collect()
    }

    /// The kube contexts the services run in, picked as for their sessions: `context_arg`
    /// (`--context`), then the service's `context`, then `common.context`. `None` stands for the
    /// kubeconfig's current context.
    pub fn service_contexts(&self, context_arg: Option<Arc<str>>) -> BTreeSet<Option<Arc<str>>> {
        let up_context = UpKubeContext {
            command_arg: context_arg,
            common_context: self.common.context.clone(),
        };
        self.services
            .values()
            .map(|service| up_context.get_context(service.context.clone()))
            .collect()
    }

    /// Produces an iterator of [`UnresolvedTarget`]s that yields an
    /// item for each service entry that requires querying the cluster
    /// to resolve the workload.
    pub(crate) fn unresolved_targets(
        &self,
        up_context: UpKubeContext,
    ) -> impl Iterator<Item = UnresolvedTarget> {
        self.services
            .iter()
            .filter_map(move |(name, svc)| svc.resolve_target(name, up_context.clone()).err())
    }
}

/// Error produced by [`UpConfig::select_services`].
#[derive(Debug, Error, Diagnostic)]
pub enum SelectError {
    /// One or more named services aren't defined in the config.
    #[error("unknown service(s): {}", unknown.join(", "))]
    #[diagnostic(help("Available services: {}", available.join(", ")))]
    UnknownServices {
        /// The names that weren't found.
        unknown: Vec<String>,
        /// The service names defined in the config.
        available: Vec<String>,
    },

    /// Every service is marked `skip: true`, so a bare `mirrord up` has
    /// nothing to launch.
    #[error("no services to launch: every service is marked `skip: true`")]
    #[diagnostic(help("Name a service explicitly to run it, or remove a `skip: true` flag."))]
    AllSkipped,
}

/// The environment variable that sets the image of `service` for `mirrord preview up`:
/// `MIRRORD_<SERVICE>_IMAGE`, with the service name in upper case and every character that isn't
/// an ASCII letter or digit turned into `_` (`cake-maker` reads `MIRRORD_CAKE_MAKER_IMAGE`).
pub fn preview_image_env(service: &str) -> String {
    let service = service
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("MIRRORD_{service}_IMAGE")
}

/// Where `mirrord preview up` took a service's image from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewImageSource {
    /// `--image <service>=<ref>`.
    Flag,
    /// `MIRRORD_<SERVICE>_IMAGE`.
    Env,
    /// The service's `image` in `mirrord-up.yaml`.
    File,
}

/// What [`UpConfig::select_preview_images`] picked.
#[derive(Debug, Default)]
pub struct PreviewImages {
    /// Where the image of every service left in the config came from.
    pub sources: HashMap<Arc<str>, PreviewImageSource>,
    /// The services dropped for having no image, sorted by name.
    pub skipped: Vec<Arc<str>>,
}

/// Error produced by [`UpConfig::select_preview_images`].
#[derive(Debug, Error, Diagnostic)]
pub enum PreviewImageError {
    /// An `--image` value isn't `<service>=<ref>` with both parts set.
    #[error("invalid `--image {0}`: expected `<service>=<image ref>`")]
    InvalidFlag(String),

    /// An `--image` value names a service the config doesn't define.
    #[error("`--image` for unknown service `{service}`")]
    #[diagnostic(help("Available services: {}", available.join(", ")))]
    UnknownService {
        /// The name that wasn't found.
        service: String,
        /// The service names defined in the config.
        available: Vec<String>,
    },

    /// Two `--image` values name the same service.
    #[error("`--image` is given twice for service `{0}`")]
    DuplicateFlag(String),

    /// A set `MIRRORD_<SERVICE>_IMAGE` stands for more than one service.
    #[error("`{env}` is set, but it could be the image of any of: {}", services.join(", "))]
    #[diagnostic(help("Use `--image <service>=<image ref>` for these services instead."))]
    AmbiguousEnv {
        /// The environment variable.
        env: String,
        /// The services whose names map onto it.
        services: Vec<String>,
    },

    /// No service got an image, so there is nothing to preview.
    #[error("no service has an image to preview")]
    #[diagnostic(help(
        "Pass `--image <service>=<image ref>`, set `MIRRORD_<SERVICE>_IMAGE`, or set `image` on \
         a service in mirrord-up.yaml."
    ))]
    NoImages,
}

#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub(crate) struct UnresolvedTarget {
    pub(crate) workload_name: Arc<str>,
    pub(crate) namespace: Option<Arc<str>>,
    pub(crate) kube_context: Option<Arc<str>>,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ResolvedTarget {
    pub(crate) resolved: SpecifiedTarget,
}

impl CollectAnalytics for &CommonConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        let CommonConfig {
            accept_invalid_certificates,
            operator,
            telemetry,
            context,
        } = self;

        analytics.add(
            "accept_invalid_certificates",
            accept_invalid_certificates.is_some(),
        );
        analytics.add("operator", operator.is_some());
        analytics.add("telemetry", telemetry.is_some());
        analytics.add("context", context.is_some());
    }
}

impl CollectAnalytics for &UpConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("num_services", self.services.len());
        analytics.add("common_fields_used", &self.common);

        let mut count_target: u32 = 0;
        let mut count_env: u32 = 0;
        let mut count_default_mode: u32 = 0;
        let mut count_http_filter: u32 = 0;
        let mut count_ignore_ports: u32 = 0;
        let mut count_skip: u32 = 0;
        let mut count_context: u32 = 0;
        let mut count_config_patch: u32 = 0;
        let mut count_image: u32 = 0;
        let mut count_directory: u32 = 0;
        let mut count_pre_command: u32 = 0;

        let mut count_exec: u32 = 0;
        let mut count_container: u32 = 0;

        let mut count_mode_split: u32 = 0;
        let mut count_mode_replace: u32 = 0;
        let mut count_mode_mirror: u32 = 0;

        for svc in self.services.values() {
            let ServiceConfig {
                target,
                env,
                default_mode,
                http_filter,
                ignore_ports,
                skip,
                run,
                context,
                config_patch,
                image,
            } = svc;

            if *target != TargetConfig::UNSPECIFIED {
                count_target += 1;
            }
            if *env != EnvConfig::default() {
                count_env += 1;
            }
            if *default_mode != ServiceMode::default() {
                count_default_mode += 1;
            }
            match default_mode {
                ServiceMode::Split => count_mode_split += 1,
                ServiceMode::Replace => count_mode_replace += 1,
                ServiceMode::Mirror => count_mode_mirror += 1,
            }
            if http_filter.is_filter_set() {
                count_http_filter += 1;
            }
            if ignore_ports.is_empty().not() {
                count_ignore_ports += 1;
            }
            if *skip {
                count_skip += 1;
            }
            if context.is_some() {
                count_context += 1;
            }
            if config_patch.is_some() {
                count_config_patch += 1;
            }
            if image.is_some() {
                count_image += 1;
            }
            if run.directory.is_some() {
                count_directory += 1;
            }
            if run.pre_command.is_empty().not() {
                count_pre_command += 1;
            }

            match run.r#type {
                RunType::Exec => count_exec += 1,
                RunType::Container => count_container += 1,
            }
        }

        let mut config_fields_used = Analytics::default();
        config_fields_used.add("target", count_target);
        config_fields_used.add("env", count_env);
        config_fields_used.add("default_mode", count_default_mode);
        config_fields_used.add("http_filter", count_http_filter);
        config_fields_used.add("ignore_ports", count_ignore_ports);
        config_fields_used.add("skip", count_skip);
        config_fields_used.add("context", count_context);
        config_fields_used.add("config_patch", count_config_patch);
        config_fields_used.add("image", count_image);
        config_fields_used.add("directory", count_directory);
        config_fields_used.add("pre_command", count_pre_command);
        analytics.add("config_fields_used", config_fields_used);

        let mut run_types = Analytics::default();
        run_types.add("exec", count_exec);
        run_types.add("container", count_container);
        analytics.add("run_types", run_types);

        let mut modes = Analytics::default();
        modes.add("split", count_mode_split);
        modes.add("replace", count_mode_replace);
        modes.add("mirror", count_mode_mirror);
        analytics.add("modes", modes);
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::{collections::HashSet, str::FromStr};

    use mirrord_config::feature::split_queues::QueueKind;
    use rstest::rstest;

    use super::*;

    /// Helper: parse YAML into UpConfig via the two-layer config system.
    fn parse(yaml: &str) -> UpConfig {
        serde_saphyr::from_str(yaml).unwrap()
    }

    /// The value at the `mirrord.json` path `layer_path` of the config `mirrord up` generates for
    /// the one service of `yaml`.
    fn layer_value(yaml: &str, layer_path: &str) -> serde_json::Value {
        let config = parse(yaml);
        let up_context = UpKubeContext {
            command_arg: None,
            common_context: config.common.context.clone(),
        };
        let mut configs = config
            .service_configs(
                &EnvKey::Provided("key".to_owned()),
                &mut HashMap::new(),
                up_context,
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();
        value_at(
            &serde_json::to_value(configs.remove(0).config).unwrap(),
            layer_path,
        )
    }

    fn value_at(config: &serde_json::Value, layer_path: &str) -> serde_json::Value {
        let pointer: String = layer_path
            .split('.')
            .map(|segment| format!("/{segment}"))
            .collect();
        config.pointer(&pointer).cloned().unwrap_or_default()
    }

    /// Each setting of [`COMMON_LAYER_PATHS`] and [`SERVICE_LAYER_PATHS`] changes the
    /// `mirrord.json` option it's listed with, so the tables can't drift from what `mirrord up`
    /// does.
    #[test]
    fn layer_paths_match_the_generated_config() {
        let value = |setting: &str| match setting {
            "accept_invalid_certificates" => "true",
            "operator" | "telemetry" => "false",
            "context" => "my-context",
            "target" => "{ path: deployment/api }",
            "env" => "{ include: A }",
            "default_mode" => "mirror",
            "http_filter" => "{ header_filter: x }",
            "ignore_ports" => "[80]",
            other => panic!("no value to set `{other}` to"),
        };
        let service =
            |extra: &str| format!("services:\n  api:\n{extra}    run:\n      command: [x]\n");
        let base = service("    target: none\n");

        for (setting, layer_path) in COMMON_LAYER_PATHS {
            let changed = format!("common:\n  {setting}: {}\n{base}", value(setting));
            assert_ne!(
                layer_value(&base, layer_path),
                layer_value(&changed, layer_path),
                "common.{setting} -> {layer_path}"
            );
        }
        for (setting, layer_path) in SERVICE_LAYER_PATHS {
            let mut extra = format!("    {setting}: {}\n", value(setting));
            if *setting != "target" {
                extra.push_str("    target: none\n");
            }
            assert_ne!(
                layer_value(&base, layer_path),
                layer_value(&service(&extra), layer_path),
                "services.*.{setting} -> {layer_path}"
            );
        }
    }

    fn windows_validation_fixture() -> UpConfig {
        parse(
            r#"
            services:
              exec:
                run:
                  command: ["node"]
              zeta-container:
                run:
                  type: container
                  command: ["docker"]
              alpha-container:
                run:
                  type: container
                  command: ["docker"]
            "#,
        )
    }

    #[test]
    fn defaults_applied_when_omitted() {
        let config = parse(
            r#"
            services:
              my-svc:
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(config.common, Default::default(),);
    }

    #[test]
    fn defaults_overridden() {
        let config = parse(
            r#"
            common:
              accept_invalid_certificates: true
              operator: false
              telemetry: false
              context: "minikube"
            services:
              my-svc:
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(
            config.common,
            CommonConfig {
                accept_invalid_certificates: Some(true),
                operator: Some(false),
                telemetry: Some(false),
                context: Some("minikube".into())
            }
        );
    }

    #[test]
    fn service_with_all_fields() {
        let config = parse(
            r#"
            services:
              web:
                target:
                  path: "deployment/web-app"
                  namespace: "staging"
                env:
                  override:
                    NODE_ENV: "development"
                default_mode: split
                http_filter:
                  header_filter: "x-session: local"
                run:
                  type: container
                  command: ["docker", "run", "-p", "8080:8080", "web:latest"]
            "#,
        );
        let svc = &config.services["web"];

        assert_eq!(
            svc.target
                .unwrap_specified()
                .path
                .as_ref()
                .unwrap()
                .to_string(),
            "deployment/web-app"
        );
        assert_eq!(
            svc.target.unwrap_specified().namespace.as_deref(),
            Some("staging")
        );
        assert_eq!(
            svc.env.r#override.as_ref().unwrap()["NODE_ENV"],
            "development"
        );
        assert_eq!(svc.default_mode, ServiceMode::Split);
        assert_eq!(
            svc.http_filter.header_filter.as_deref(),
            Some("x-session: local")
        );
        assert_eq!(
            svc.run,
            RunConfig {
                r#type: RunType::Container,
                directory: None,
                command: vec![
                    "docker".into(),
                    "run".into(),
                    "-p".into(),
                    "8080:8080".into(),
                    "web:latest".into(),
                ],
                pre_command: Vec::new(),
            }
        );
    }

    #[test]
    fn windows_validation_accepts_exec_without_ci() {
        let mut config = windows_validation_fixture();
        config.select_services(&["exec".to_owned()]).unwrap();

        config.validate_windows(false).unwrap();
    }

    #[test]
    fn windows_validation_ignores_skipped_container_services() {
        let mut config = parse(
            r#"
            services:
              exec:
                run:
                  command: ["node"]
              skipped-container:
                skip: true
                run:
                  type: container
                  command: ["docker"]
            "#,
        );
        config.select_services(&[]).unwrap();

        config.validate_windows(false).unwrap();
    }

    #[test]
    fn windows_validation_rejects_selected_container_services() {
        let mut config = windows_validation_fixture();
        config
            .select_services(&["exec".to_owned(), "zeta-container".to_owned()])
            .unwrap();

        assert!(matches!(
            config.validate_windows(false),
            Err(WindowsSupportError::Container(name)) if &*name == "zeta-container"
        ));
    }

    #[test]
    fn windows_validation_rejects_ci_for_exec_services() {
        let mut config = windows_validation_fixture();
        config.select_services(&["exec".to_owned()]).unwrap();

        assert!(matches!(
            config.validate_windows(true),
            Err(WindowsSupportError::Ci)
        ));
    }

    #[test]
    fn multiple_services_with_different_modes() {
        let config = parse(
            r#"
            services:
              svc-a:
                run:
                  type: exec
                  directory: services/a
                  command: ["node", "a.js"]
              svc-b:
                run:
                  command: ["node", "b.js"]
            "#,
        );
        assert_eq!(config.services.len(), 2);

        for service in config.services.values() {
            assert_eq!(service.run.r#type, RunType::Exec);
        }
        assert_eq!(
            config.services["svc-a"].run.directory.as_deref(),
            Some(std::path::Path::new("services/a"))
        );
        assert_eq!(config.services["svc-b"].run.directory, None);
    }

    #[test]
    fn minimal_service_gets_defaults() {
        let config = parse(
            r#"
            services:
              svc:
                run:
                  command: ["echo"]
            "#,
        );
        let svc = &config.services["svc"];
        assert_eq!(svc.env, EnvConfig::default());
    }

    #[test]
    fn split_mode_injects_session_key_wildcard_queue_filters() {
        let config = parse(
            r#"
            common:
              operator: true
            services:
              worker:
                target:
                  path: "deployment/sqs-printer"
                run:
                  command: ["../target/debug/rust-sqs-printer"]
            "#,
        );

        let mut services = config
            .service_configs(
                &EnvKey::Provided("sqs-session".to_owned()),
                &mut HashMap::new(),
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();
        assert_eq!(services.len(), 1);

        let service = services.pop().unwrap();
        assert_eq!(service.config.operator, Some(true));
        assert!(
            service
                .config
                .feature
                .split_queues
                .is_all_wildcard_default_mode(&EnvKey::Provided("sqs-session".to_owned()))
        );
    }

    #[test]
    fn mirror_mode_injects_mirror_queue_filters() {
        let config = parse(
            r#"
                common:
                  operator: true
                services:
                  worker:
                    default_mode: mirror
                    target:
                      path: "deployment/sqs-printer"
                    run:
                      command: ["../target/debug/rust-sqs-printer"]
                "#,
        );

        let mut services = config
            .service_configs(
                &EnvKey::Provided("sqs-mirror".to_owned()),
                &mut HashMap::new(),
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();
        assert_eq!(services.len(), 1);

        let service = services.pop().unwrap();
        assert_eq!(service.config.operator, Some(true));
        assert!(
            service
                .config
                .feature
                .split_queues
                .is_all_wildcard_with_mode(
                    &EnvKey::Provided("sqs-mirror".to_owned()),
                    QueueMode::Mirror
                )
        );
    }

    #[test]
    fn config_patch_overrides_generated_split_queues_and_renders_key() {
        let key = EnvKey::Provided("jadwiga".to_owned());
        let mut services = crate::template(
            r#"
            services:
              worker:
                target:
                  path: "deployment/sqs-printer"
                config_patch:
                  feature:
                    split_queues:
                      "*":
                        queue_type: SQS
                        jq_filter: '.Body | fromjson | .headers["x-origin"] == "{{ key }}"'
                run:
                  command: ["echo"]
            "#,
            &key,
        )
        .unwrap()
        .service_configs(
            &key,
            &mut HashMap::new(),
            UpKubeContext::default(),
            &mut ConfigContext::default().strict_env(true),
        )
        .unwrap();

        let service = services.pop().unwrap();
        assert_eq!(
            service
                .config
                .feature
                .split_queues
                .of_kind(QueueKind::Sqs)
                .map(|split| (split.queue_id.as_str(), split.jq_filter.as_deref()))
                .collect::<Vec<_>>(),
            [(
                "*",
                Some(r#".Body | fromjson | .headers["x-origin"] == "jadwiga""#)
            )]
        );
    }

    #[test]
    fn config_patch_schema_error_names_service_and_actual_error() {
        let error = parse(
            r#"
            services:
              worker:
                target: none
                config_patch:
                  feature:
                    split_queues: NOT_A_SPLIT_QUEUE_CONFIG
                run:
                  command: ["echo"]
            "#,
        )
        .service_configs(
            &EnvKey::Provided("jagiellon".to_owned()),
            &mut HashMap::new(),
            UpKubeContext::default(),
            &mut ConfigContext::default().strict_env(true),
        )
        .err()
        .expect("invalid split queue shape should fail schema validation");

        let error = error.to_string();
        assert!(error.contains("service `worker`"), "{error}");
        assert!(
            error.contains("a map from queue id to its filter, or a list of queue split entries"),
            "{error}"
        );
    }

    #[test]
    fn config_patch_semantic_error_names_service_and_actual_error() {
        let error = parse(
            r#"
            services:
              worker:
                target: none
                config_patch:
                  feature:
                    split_queues:
                      "*":
                        queue_type: SQS
                        jq_filter: "["
                run:
                  command: ["echo"]
            "#,
        )
        .service_configs(
            &EnvKey::Provided("jadwiga".to_owned()),
            &mut HashMap::new(),
            UpKubeContext::default(),
            &mut ConfigContext::default().strict_env(true),
        )
        .err()
        .expect("invalid jq should fail config validation");

        let error = error.to_string();
        assert!(error.contains("service `worker`"), "{error}");
        assert!(
            error.contains("Queue splitting config is invalid"),
            "{error}"
        );
    }

    #[test]
    fn replace_mode_parses_from_config_file() {
        let config = parse(
            r#"
            services:
              svc:
                default_mode: replace
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(config.services["svc"].default_mode, ServiceMode::Replace);
    }

    #[test]
    fn replace_mode_copies_target_and_scales_down() {
        let mut services = parse(
            r#"
            services:
              web:
                default_mode: replace
                target:
                  path: "deployment/web"
                run:
                  command: ["echo"]
            "#,
        )
        .service_configs(
            &EnvKey::Provided("a-session".to_owned()),
            &mut HashMap::new(),
            UpKubeContext::default(),
            &mut ConfigContext::default().strict_env(true),
        )
        .unwrap();
        assert_eq!(services.len(), 1);
        let service = services.pop().unwrap();

        assert_eq!(
            service.config.feature.copy_target,
            CopyTargetConfig {
                enabled: true,
                scale_down: true,
                exclude_containers: Vec::new(),
                exclude_init_containers: Vec::new(),
            }
        );
        assert_eq!(
            service.config.feature.network.incoming.mode,
            IncomingMode::Steal
        );
    }

    #[test]
    fn replace_mode_clears_the_http_filter() {
        let mut services = parse(
            r#"
            services:
              web:
                default_mode: replace
                target:
                  path: "deployment/web"
                http_filter:
                  header_filter: "x-mirrord-user: test"
                run:
                  command: ["echo"]
            "#,
        )
        .service_configs(
            &EnvKey::Provided("a-session".to_owned()),
            &mut HashMap::new(),
            UpKubeContext::default(),
            &mut ConfigContext::default().strict_env(true),
        )
        .unwrap();
        assert_eq!(services.len(), 1);
        let service = services.pop().unwrap();

        assert!(
            service
                .config
                .feature
                .network
                .incoming
                .http_filter
                .is_filter_set()
                .not()
        );
    }

    #[test]
    fn override_mode_replaces_every_service_mode() {
        let mut config = parse(
            r#"
            services:
              a:
                default_mode: split
                run:
                  command: ["echo"]
              b:
                default_mode: replace
                run:
                  command: ["echo"]
              c:
                run:
                  command: ["echo"]
            "#,
        );

        config.override_mode(ServiceMode::Replace);
        assert!(
            config
                .services
                .values()
                .all(|svc| svc.default_mode == ServiceMode::Replace)
        );

        config.override_mode(ServiceMode::Split);
        assert!(
            config
                .services
                .values()
                .all(|svc| svc.default_mode == ServiceMode::Split)
        );

        config.override_mode(ServiceMode::Mirror);
        assert!(
            config
                .services
                .values()
                .all(|svc| svc.default_mode == ServiceMode::Mirror)
        );
    }

    #[test]
    fn validate_targets_accepts_replace_on_supported_targets() {
        for path in ["deployment/web", "statefulset/web", "replicaset/web"] {
            let services = parse(&format!(
                r#"
                services:
                  web:
                    default_mode: replace
                    target:
                      path: "{path}"
                    run:
                      command: ["echo"]
                "#,
            ))
            .service_configs(
                &EnvKey::Provided("a-session".to_owned()),
                &mut HashMap::new(),
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();

            validate_targets(&services).unwrap_or_else(|err| panic!("{path} rejected: {err}"));
        }
    }

    #[test]
    fn validate_targets_reports_every_offending_service() {
        let services = parse(
            r#"
            services:
              a:
                default_mode: replace
                target: none
                run:
                  command: ["echo"]
              b:
                default_mode: replace
                target:
                  path: "deployment/ok"
                run:
                  command: ["echo"]
              c:
                default_mode: replace
                target:
                  path: "rollout/web"
                run:
                  command: ["echo"]
            "#,
        )
        .service_configs(
            &EnvKey::Provided("a-session".to_owned()),
            &mut HashMap::new(),
            UpKubeContext::default(),
            &mut ConfigContext::default().strict_env(true),
        )
        .unwrap();

        let ModeError::IncompatibleTargets(incompatible) =
            validate_targets(&services).expect_err("`a` and `c` are not usable in replace mode");

        let mut names = incompatible
            .iter()
            .map(|target| target.service.to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["a", "c"]);
    }

    #[test]
    fn specified_target_parses_correctly() {
        let config = parse(
            r#"
            services:
              svc:
                target:
                  path: "pod/my-pod/container/main"
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(
            config.services["svc"]
                .target
                .unwrap_specified()
                .path
                .as_ref()
                .unwrap()
                .to_string(),
            "pod/my-pod/container/main"
        );
    }

    #[test]
    fn unspecified_target_parses_correctly() {
        let config = parse(
            r#"
            services:
              svc:
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(config.services["svc"].target, TargetConfig::UNSPECIFIED);
    }

    /// Every [`TargetConfig`] state must survive serialize → parse
    /// without relying on the wizard's null-pruning in between.
    #[test]
    fn target_round_trips() {
        for target in [
            TargetConfig::Targetless,
            TargetConfig::UNSPECIFIED,
            SpecifiedTarget {
                path: Some(Target::from_str("deployment/web").unwrap()),
                namespace: Some("staging".into()),
            }
            .into(),
            SpecifiedTarget {
                path: None,
                namespace: Some("staging".into()),
            }
            .into(),
        ] {
            let yaml = serde_saphyr::to_string(&target).unwrap();
            let parsed: TargetConfig = serde_saphyr::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{yaml:?} failed to parse: {e}"));
            assert_eq!(parsed, target, "round-trip through {yaml:?}");
        }
    }

    #[test]
    fn none_target_parses_correctly() {
        let config = parse(
            r#"
            services:
              svc:
                target: none
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(config.services["svc"].target, TargetConfig::Targetless);
    }

    // -- Context resolution --

    #[rstest]
    #[case("cli-arg-set", Some("cli-context"), Some("cli-context"))]
    #[case("per-service-set", None, Some("service-context"))]
    #[case("common-set", None, Some("common-context"))]
    fn unresolved_target_chooses_correct_context(
        #[case] service_name: &str,
        #[case] cli_context_arg: Option<&str>,
        #[case] expected_kube_context: Option<&str>,
    ) {
        let config = parse(
            r#"
            common:
              context: common-context
            services:
              cli-arg-set:
                context: service-context
                run:
                  command: ["echo"]
              per-service-set:
                context: service-context
                run:
                  command: ["echo"]
              common-set:
                run:
                  command: ["echo"]
            "#,
        );

        let up_context = UpKubeContext {
            command_arg: cli_context_arg.map(Into::into),
            common_context: config.common.context.clone(),
        };

        assert_eq!(
            config
                .unresolved_targets(up_context)
                .find(|target| target.workload_name.as_ref() == service_name)
                .unwrap()
                .kube_context
                .as_deref(),
            expected_kube_context
        );
    }

    // -- Target resolution --

    fn env_key() -> EnvKey {
        EnvKey::Provided("test-key".to_owned())
    }

    fn unresolved_target(
        workload_name: &str,
        namespace: Option<&str>,
        kube_context: Option<&str>,
    ) -> UnresolvedTarget {
        UnresolvedTarget {
            workload_name: workload_name.into(),
            namespace: namespace.map(Into::into),
            kube_context: kube_context.map(Into::into),
        }
    }

    fn resolved_target(path: &str, namespace: Option<&str>) -> ResolvedTarget {
        ResolvedTarget {
            resolved: SpecifiedTarget {
                path: Some(Target::from_str(path).unwrap()),
                namespace: namespace.map(Into::into),
            },
        }
    }

    /// One service in every target state: explicit path, `none`,
    /// inferred in the default namespace, inferred in an explicit
    /// namespace.
    fn resolution_fixture() -> UpConfig {
        parse(
            r#"
            services:
              explicit:
                target:
                  path: "deployment/web"
                  namespace: "prod"
                run:
                  command: ["echo", "explicit"]
              opted-out:
                target: none
                run:
                  command: ["echo", "opted-out"]
              inferred:
                run:
                  command: ["echo", "inferred"]
              inferred-ns:
                target:
                  namespace: "staging"
                run:
                  command: ["echo", "inferred-ns"]
            "#,
        )
    }

    /// Only services without a `target.path` need cluster resolution;
    /// the service key becomes the workload name and the namespace
    /// hint (if any) is carried along.
    #[test]
    fn unresolved_targets_yields_only_services_needing_resolution() {
        let config = resolution_fixture();

        assert_eq!(
            config
                .unresolved_targets(UpKubeContext::default())
                .collect::<HashSet<_>>(),
            HashSet::from([
                unresolved_target("inferred", None, None),
                unresolved_target("inferred-ns", Some("staging"), None),
            ]),
        );
    }

    #[test]
    fn unresolved_targets_empty_when_no_service_needs_resolution() {
        let config = parse(
            r#"
            services:
              explicit:
                target:
                  path: "deployment/web"
                run:
                  command: ["echo"]
              opted-out:
                target: none
                run:
                  command: ["echo"]
            "#,
        );

        assert_eq!(
            config.unresolved_targets(UpKubeContext::default()).count(),
            0
        );
    }

    /// Explicit and `none` targets pass through as-is, inferred ones
    /// take their path/namespace from `resolved_targets`, and the used
    /// entries are drained from the map.
    #[test]
    fn service_configs_resolves_and_drains() {
        let config = resolution_fixture();
        let key = env_key();
        let mut resolved_targets = HashMap::from([
            (
                unresolved_target("inferred", None, None),
                resolved_target("deployment/inferred", Some("default")),
            ),
            (
                unresolved_target("inferred-ns", Some("staging"), None),
                resolved_target("pod/inferred-ns-7f9d8c7df8", Some("staging")),
            ),
            // Not requested by any service; must survive the drain.
            (
                unresolved_target("unrelated", None, None),
                resolved_target("deployment/unrelated", None),
            ),
        ]);

        let subprocesses: HashMap<String, SubprocessCfg> = config
            .service_configs(
                &key,
                &mut resolved_targets,
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap()
            .into_iter()
            .map(|cfg| (cfg.service_name.to_string(), cfg))
            .collect();

        assert_eq!(subprocesses.len(), 4);

        let explicit = &subprocesses["explicit"].config.target;
        assert_eq!(
            explicit.path.as_ref().unwrap().to_string(),
            "deployment/web"
        );
        assert_eq!(explicit.namespace.as_deref(), Some("prod"));

        let opted_out = &subprocesses["opted-out"].config.target;
        assert_eq!(opted_out.path, None);
        assert_eq!(opted_out.namespace, None);

        let inferred = &subprocesses["inferred"].config.target;
        assert_eq!(
            inferred.path.as_ref().unwrap().to_string(),
            "deployment/inferred"
        );
        assert_eq!(inferred.namespace.as_deref(), Some("default"));

        let inferred_ns = &subprocesses["inferred-ns"].config.target;
        assert_eq!(
            inferred_ns.path.as_ref().unwrap().to_string(),
            "pod/inferred-ns-7f9d8c7df8"
        );
        assert_eq!(inferred_ns.namespace.as_deref(), Some("staging"));

        // The run config and session key pass through untouched.
        assert_eq!(
            subprocesses["inferred"].run.command,
            vec!["echo".to_owned(), "inferred".to_owned()]
        );
        assert_eq!(subprocesses["inferred"].config.key, key);

        assert_eq!(
            resolved_targets.into_keys().collect::<Vec<_>>(),
            vec![unresolved_target("unrelated", None, None)],
        );
    }

    #[test]
    #[should_panic(expected = "No resolved entry for `inferred`")]
    fn service_configs_panics_on_missing_resolved_entry() {
        let config = parse(
            r#"
            services:
              inferred:
                run:
                  command: ["echo"]
            "#,
        );
        let key = env_key();
        let mut resolved_targets = HashMap::new();

        config
            .service_configs(
                &key,
                &mut resolved_targets,
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();
    }

    /// The resolution step must key its answers by the exact
    /// [`UnresolvedTarget`] it was asked about -- an entry for the
    /// right workload under the wrong namespace does not count.
    #[test]
    #[should_panic(expected = "No resolved entry for `inferred-ns`")]
    fn service_configs_panics_on_namespace_mismatch() {
        let config = parse(
            r#"
            services:
              inferred-ns:
                target:
                  namespace: "staging"
                run:
                  command: ["echo"]
            "#,
        );
        let key = env_key();
        let mut resolved_targets = HashMap::from([(
            unresolved_target("inferred-ns", None, None),
            resolved_target("deployment/inferred-ns", Some("staging")),
        )]);

        config
            .service_configs(
                &key,
                &mut resolved_targets,
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "must have a non-null path")]
    fn service_configs_panics_on_resolved_entry_without_path() {
        let config = parse(
            r#"
            services:
              inferred:
                run:
                  command: ["echo"]
            "#,
        );
        let key = env_key();
        let mut resolved_targets = HashMap::from([(
            unresolved_target("inferred", None, None),
            ResolvedTarget {
                resolved: SpecifiedTarget {
                    path: None,
                    namespace: None,
                },
            },
        )]);

        config
            .service_configs(
                &key,
                &mut resolved_targets,
                UpKubeContext::default(),
                &mut ConfigContext::default().strict_env(true),
            )
            .unwrap();
    }

    // -- Service selection --

    fn selection_fixture() -> UpConfig {
        parse(
            r#"
            services:
              a:
                run:
                  command: ["echo", "a"]
              b:
                skip: true
                run:
                  command: ["echo", "b"]
              c:
                run:
                  command: ["echo", "c"]
            "#,
        )
    }

    fn service_names(config: &UpConfig) -> HashSet<String> {
        config.services.keys().map(|k| k.to_string()).collect()
    }

    #[test]
    fn no_selection_drops_skipped_services() {
        let mut config = selection_fixture();
        config.select_services(&[]).unwrap();
        assert_eq!(
            service_names(&config),
            HashSet::from(["a".to_owned(), "c".to_owned()]),
        );
    }

    #[test]
    fn selection_keeps_only_named_services() {
        let mut config = selection_fixture();
        config.select_services(&["a".to_owned()]).unwrap();
        assert_eq!(service_names(&config), HashSet::from(["a".to_owned()]));
    }

    #[test]
    fn explicit_selection_overrides_skip() {
        let mut config = selection_fixture();
        config.select_services(&["b".to_owned()]).unwrap();
        assert_eq!(service_names(&config), HashSet::from(["b".to_owned()]));
    }

    #[test]
    fn unknown_service_errors_and_lists_available() {
        let mut config = selection_fixture();
        let err = config
            .select_services(&["nope".to_owned(), "a".to_owned()])
            .unwrap_err();
        match err {
            SelectError::UnknownServices { unknown, available } => {
                assert_eq!(unknown, vec!["nope".to_owned()]);
                assert_eq!(available, vec!["a", "b", "c"]);
            }
            other => panic!("expected UnknownServices, got {other:?}"),
        }
    }

    #[test]
    fn no_selection_with_all_skipped_errors() {
        let mut config = parse(
            r#"
            services:
              only:
                skip: true
                run:
                  command: ["echo"]
            "#,
        );
        assert!(matches!(
            config.select_services(&[]).unwrap_err(),
            SelectError::AllSkipped
        ));
    }

    // -- Error cases --

    #[test]
    fn error_missing_run() {
        let err = serde_saphyr::from_str::<UpConfig>(
            r#"
            services:
              svc:
                default_mode: split
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("missing field `run`"),
            "expected error about missing run, got: {err}"
        );
    }

    #[test]
    fn error_invalid_mode() {
        let result: Result<UpConfig, _> = serde_saphyr::from_str(
            r#"
            services:
              svc:
                default_mode: bogus
                run:
                  exec:
                    command: ["echo"]
            "#,
        );
        assert!(result.is_err());
    }

    /// A typo of `target: none` gets `none` added to the message as a hint.
    #[rstest]
    #[case::typo("nnoe", "; did you mean `none`?")]
    #[case::uppercase("NONE", "; did you mean `none`?")]
    #[case::unrelated("deployment", "")]
    fn error_invalid_target_string(#[case] target: &str, #[case] hint: &str) {
        let err = serde_saphyr::from_str::<UpConfig>(&format!(
            "services:\n  svc:\n    target: {target}\n    run:\n      command: [x]\n"
        ))
        .unwrap_err();
        let expected = format!(
            "line 3 column 5: invalid value: string \"{target}\", expected `none`, a target \
             mapping, or nothing at all{hint}\n"
        );
        assert!(
            err.to_string().contains(&expected),
            "no `{expected}` in: {err}"
        );
    }

    // -- Analytics --

    fn collect(config: &UpConfig) -> serde_json::Value {
        let mut analytics = Analytics::default();
        config.collect_analytics(&mut analytics);
        serde_json::to_value(&analytics).unwrap()
    }

    #[test]
    fn analytics_minimal_config_reports_zero_counts() {
        let config = parse(
            r#"
            services:
              svc:
                run:
                  command: ["echo"]
            "#,
        );

        assert_json_diff::assert_json_eq!(
            collect(&config),
            serde_json::json!({
                "num_services": 1,
                "common_fields_used": {
                    "accept_invalid_certificates": false,
                    "operator": false,
                    "telemetry": false,
                    "context": false,
                },
                "config_fields_used": {
                    "target": 0,
                    "env": 0,
                    "default_mode": 0,
                    "http_filter": 0,
                    "ignore_ports": 0,
                    "skip": 0,
                    "context": 0,
                    "config_patch": 0,
                    "image": 0,
                    "directory": 0,
                    "pre_command": 0
                },
                "run_types": {
                    "exec": 1,
                    "container": 0,
                },
                "modes": {
                    "split": 1,
                    "replace": 0,
                    "mirror": 0,
                },
            }),
        );
    }

    #[test]
    fn analytics_common_fields_tracked_individually() {
        let config = parse(
            r#"
            common:
              accept_invalid_certificates: true
              telemetry: false
            services:
              svc:
                run:
                  command: ["echo"]
            "#,
        );

        let result = collect(&config);
        assert_eq!(
            result["common_fields_used"]["accept_invalid_certificates"],
            true
        );
        assert_eq!(result["common_fields_used"]["operator"], false);
        assert_eq!(result["common_fields_used"]["telemetry"], true);
    }

    #[test]
    fn analytics_service_fields_aggregated_across_services() {
        // Three services: svc-a sets target + ignore_ports, svc-b sets env + http_filter,
        // svc-c sets target only. Counts should reflect the per-field population density
        // (e.g. `target: 2`, `http_filter: 1`).
        let config = parse(
            r#"
            services:
              svc-a:
                target:
                  path: "deployment/web-app"
                ignore_ports: [8080]
                run:
                  command: ["echo"]
              svc-b:
                context: "magical-mystery-cluster"
                env:
                  override:
                    KEY: "value"
                http_filter:
                  header_filter: "x-custom: foo"
                run:
                  type: container
                  command: ["docker", "run", "img"]
              svc-c:
                target:
                  path: "deployment/other"
                config_patch:
                  feature:
                    hostname: false
                run:
                  command: ["echo"]
            "#,
        );

        let result = collect(&config);
        assert_eq!(result["num_services"], 3);
        assert_eq!(result["config_fields_used"]["target"], 2);
        assert_eq!(result["config_fields_used"]["env"], 1);
        assert_eq!(result["config_fields_used"]["http_filter"], 1);
        assert_eq!(result["config_fields_used"]["ignore_ports"], 1);
        assert_eq!(result["config_fields_used"]["default_mode"], 0);
        assert_eq!(result["config_fields_used"]["context"], 1);
        assert_eq!(result["config_fields_used"]["config_patch"], 1);
        assert_eq!(result["run_types"]["exec"], 2);
        assert_eq!(result["run_types"]["container"], 1);
    }
}

#[cfg(test)]
mod schema {
    use super::UpConfig;

    /// The `mirrord-up.yaml` schema at the repo root, next to `mirrord-schema.json`.
    const SCHEMA_FILE_PATH: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../mirrord-up-schema.json");

    fn generate() -> String {
        serde_json::to_string_pretty(&schemars::schema_for!(UpConfig))
            .expect("Failed generating schema!")
    }

    /// Writes the `mirrord-up.yaml` schema to `mirrord-up-schema.json`.
    ///
    /// Run it after **ANY** change to the config types, including doc-only changes:
    ///
    /// ```sh
    /// cargo test -p mirrord-up generate_up_schema -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn generate_up_schema() {
        std::fs::write(SCHEMA_FILE_PATH, generate()).expect("Failed writing schema file!");
    }

    #[test]
    fn up_schema_file_is_up_to_date() {
        let existing = std::fs::read_to_string(SCHEMA_FILE_PATH)
            .expect("Schema file doesn't exist!")
            .replace("\r\n", "\n");

        assert!(
            existing == generate(),
            "mirrord-up-schema.json is outdated, regenerate it with \
             `cargo test -p mirrord-up generate_up_schema -- --ignored`"
        );
    }
}
