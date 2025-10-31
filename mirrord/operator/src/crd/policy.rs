use std::collections::HashSet;

use kube::CustomResource;
use schemars::{
    JsonSchema,
    schema::{ArrayValidation, InstanceType, ObjectValidation, Schema, SchemaObject, SingleOrVec},
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{IntoDeserializer, MapAccess, Visitor},
    ser::SerializeMap,
};

use super::label_selector::LabelSelector;

/// Features and operations that can be blocked by `mirrordpolicies` and `mirrordclusterpolicies`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")] // StealWithoutFilter -> steal-without-filter in yaml.
pub enum BlockedFeature {
    /// Blocks stealing traffic in any way (without or without filter).
    Steal,

    /// Blocks stealing traffic without specifying (any) filter. Client can still specify a
    /// filter that matches anything.
    StealWithoutFilter,

    /// Blocks mirroring traffic.
    Mirror,

    /// Blocks copy_target when used with scale_down.
    CopyTargetScaleDown,

    /// So that the operator is able to list all policies with [`kube::Api`],
    /// even if it doesn't recognize blocked features used in some of them.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Custom resource for policies that limit what mirrord features users can use.
///
/// This policy applies only to resources living in the same namespace.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want policies to be handled by k8s.
    group = "policies.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordPolicy",
    namespaced
)]
#[serde(rename_all = "camelCase")] // target_path -> targetPath in yaml.
pub struct MirrordPolicySpec {
    /// Specify the targets for which this policy applies, in the pod/my-pod deploy/my-deploy
    /// notation. Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this policy does not depend on the target's path.
    pub target_path: Option<String>,

    /// If specified in a policy, the policy will only apply to targets with labels that match all
    /// of the selector's rules.
    pub selector: Option<LabelSelector>,

    // TODO: make the k8s list type be set/map to prevent duplicates.
    /// List of features and operations blocked by this policy.
    pub block: Vec<BlockedFeature>,

    /// Controls how mirrord-operator handles user requests to fetch environment variables from the
    /// target.
    #[serde(default)]
    pub env: EnvPolicy,

    /// Overrides fs ops behaviour, granting control over them to the operator policy, instead of
    /// the user config.
    #[serde(default)]
    pub fs: FsPolicy,

    /// Fine grained control over network features like specifying required HTTP filters.
    #[serde(default)]
    pub network: NetworkPolicy,

    /// Enforces that the user selects a mirrord profile for their session.
    ///
    /// Mind that mirrord profiles are only a functional feature.
    /// mirrord Operator is not able to enforce that the
    /// application running on the user's machine follows the selected profile.
    ///
    /// This setting should not be used in order to prevent malicious actions.
    ///
    /// Defaults to `false`.
    #[serde(default)]
    pub require_profile: bool,

    /// A list of allowed mirrord profiles.
    ///
    /// If multiple policies apply to a session,
    /// user's mirrord profile must be present in all allowlists.
    ///
    /// Mind that mirrord profiles are only a functional feature.
    /// mirrord Operator is not able to enforce that the
    /// application running on the user's machine follows the selected profile.
    ///
    /// This setting should not be used in order to prevent malicious actions.
    ///
    /// Optional.
    pub profile_allowlist: Option<Vec<String>>,

    /// Whether this policy applies also to sessions using the copy target feature.
    ///
    /// By default, policies don't apply to copy target sessions.
    ///
    /// Defaults to `false`.
    #[serde(default)]
    pub applies_to_copy_targets: bool,

    #[serde(default)]
    pub split_queues: SplitQueuesPolicy,
}

/// Custom cluster-wide resource for policies that limit what mirrord features users can use.
///
/// This policy applies to resources across all namespaces in the cluster.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want policies to be handled by k8s.
    group = "policies.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterPolicy"
)]
#[serde(rename_all = "camelCase")] // target_path -> targetPath in yaml.
pub struct MirrordClusterPolicySpec {
    /// Specify the targets for which this policy applies, in the pod/my-pod deploy/my-deploy
    /// notation. Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this policy does not depend on the target's path.
    pub target_path: Option<String>,

    /// If specified in a policy, the policy will only apply to targets with labels that match all
    /// of the selector's rules.
    pub selector: Option<LabelSelector>,

    // TODO: make the k8s list type be set/map to prevent duplicates.
    /// List of features and operations blocked by this policy.
    pub block: Vec<BlockedFeature>,

    /// Controls how mirrord-operator handles user requests to fetch environment variables from the
    /// target.
    #[serde(default)]
    pub env: EnvPolicy,

    /// Overrides fs ops behaviour, granting control over them to the operator policy, instead of
    /// the user config.
    #[serde(default)]
    pub fs: FsPolicy,

    #[serde(default)]
    pub network: NetworkPolicy,

    /// Enforces that the user selects a mirrord profile for their session.
    ///
    /// Defaults to `false`.
    #[serde(default)]
    pub require_profile: bool,

    /// A list of allowed mirrord profiles.
    ///
    /// If multiple policies apply to a session,
    /// user's mirrord profile must be present in all allowlists.
    ///
    /// Optional.
    pub profile_allowlist: Option<Vec<String>>,

    /// Whether this policy applies also to sessions using the copy target feature.
    ///
    /// By default, policies don't apply to copy target sessions.
    ///
    /// Defaults to `false`.
    #[serde(default)]
    pub applies_to_copy_targets: bool,

    #[serde(default)]
    pub split_queues: SplitQueuesPolicy,
}

/// Policy for controlling environment variables access from mirrord instances.
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvPolicy {
    /// List of environment variables that should be excluded when using mirrord.
    ///
    /// These environment variables won't be retrieved from the target even if the user
    /// specifies them in their `feature.env.include` mirrord config.
    ///
    /// Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
    /// any character and `*` matches arbitrary many (including zero) occurrences of any character,
    /// e.g. `DATABASE_*` will match `DATABASE_URL` and `DATABASE_PORT`.
    #[serde(default)]
    pub exclude: HashSet<String>,
}

/// File operations policy that mimics the mirrord fs config.
///
/// Allows the operator control over remote file ops behaviour, overriding what the user has set in
/// their mirrord config file, if it matches something in one of the lists (regex sets) of this
/// struct.
///
/// If the file path matches regexes in multiple sets, priority is as follows:
/// 1. `local`
/// 2. `notFound`
/// 3. `readOnly`
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FsPolicy {
    /// Files that cannot be opened for writing.
    ///
    /// Opening the file for writing is rejected with an IO error.
    #[serde(default)]
    pub read_only: HashSet<String>,

    /// Files that cannot be opened at all.
    ///
    /// Opening the file will be rejected and mirrord will open the file locally instead.
    #[serde(default)]
    pub local: HashSet<String>,

    /// Files that cannot be opened at all.
    ///
    /// Opening the file is rejected with an IO error.
    #[serde(default)]
    pub not_found: HashSet<String>,
}

/// Network operations policy that partialy mimics the mirrord network config.
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicy {
    #[serde(default)]
    pub incoming: IncomingNetworkPolicy,
}

/// Incoming network operations policy that partialy mimics the mirrord `network.incoming` config.
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IncomingNetworkPolicy {
    #[serde(default)]
    pub http_filter: HttpFilterPolicy,
}

/// Http filter policy that allows to specify requirements for the HTTP filter used in a session.
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HttpFilterPolicy {
    /// Require the user's header filter to match this regex, if such filter is provided.
    ///
    /// This auto enables `steal-without-filter` policy block to require that the user specifies a
    /// header filter for the network steal feature.
    ///
    /// # Composed filters
    ///
    /// When the user requests an `all_of` HTTP filter, at least one of the nested filters
    /// must be a header filter that matches this regex. At least one nested filter is required.
    ///
    /// When the user requests an `any_of` HTTP filter, all nested header filters must match this
    /// regex. At least one nested header filter is required.
    pub header_filter: Option<String>,
}

/// Split queues policy that defines requirements for mirrord `feature.split_queues` config.
#[derive(Clone, Default, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SplitQueuesPolicy {
    /// When set to true, require a message filter to be used in split queues.
    #[serde(default)]
    pub require_filter: bool,
    /// A set of queue filter policies that need to be satisfied in a mirrord session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<QueueFilterPolicy>,
}

/// Filter rules for matching queues. Queues are matched using `queue_id` regex
/// and `queue_type`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueFilterPolicy {
    /// A regex used for matching queue ids.
    pub queue_id: String,
    /// The queue type this policy applies to.
    pub queue_type: QueueType,
    /// A set of rules for filters used against the matching queues.
    pub filter_rules: QueueFilterRuleExp,
}

/// Type of queues supported by queue filter policy.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, Eq, PartialEq, JsonSchema)]
pub enum QueueType {
    #[serde(rename = "SQS")]
    Sqs,
    #[serde(rename = "Kafka")]
    Kafka,
}

/// A recursive queue filter rule expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueFilterRuleExp {
    AllOf(Vec<QueueFilterRuleExp>),
    AnyOf(Vec<QueueFilterRuleExp>),
    Single(QueueFilterRule),
}

impl Serialize for QueueFilterRuleExp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            QueueFilterRuleExp::AllOf(exprs) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("allOf", exprs)?;
                map.end()
            }
            QueueFilterRuleExp::AnyOf(exprs) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("anyOf", exprs)?;
                map.end()
            }
            QueueFilterRuleExp::Single(rule) => rule.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for QueueFilterRuleExp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ExpVisitor;

        impl<'de> Visitor<'de> for ExpVisitor {
            type Value = QueueFilterRuleExp;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a queue filter expression (allOf, anyOf, or a single rule)")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                // Peek the first key
                let key: Option<String> = map.next_key()?;
                match key {
                    Some(ref k) if k == "allOf" => {
                        let val = map.next_value()?;
                        Ok(QueueFilterRuleExp::AllOf(val))
                    }
                    Some(ref k) if k == "anyOf" => {
                        let val = map.next_value()?;
                        Ok(QueueFilterRuleExp::AnyOf(val))
                    }
                    Some(k) => {
                        // Reconstruct the map with the first key replayed
                        struct MapReplay<'a, M> {
                            first_key: Option<String>,
                            map: &'a mut M,
                        }

                        impl<'de, M> MapAccess<'de> for MapReplay<'_, M>
                        where
                            M: MapAccess<'de>,
                        {
                            type Error = M::Error;

                            fn next_key_seed<K>(
                                &mut self,
                                seed: K,
                            ) -> Result<Option<K::Value>, M::Error>
                            where
                                K: serde::de::DeserializeSeed<'de>,
                            {
                                if let Some(k) = self.first_key.take() {
                                    seed.deserialize(k.into_deserializer()).map(Some)
                                } else {
                                    self.map.next_key_seed(seed)
                                }
                            }

                            fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, M::Error>
                            where
                                V: serde::de::DeserializeSeed<'de>,
                            {
                                self.map.next_value_seed(seed)
                            }
                        }

                        let mut replay = MapReplay {
                            first_key: Some(k),
                            map: &mut map,
                        };

                        let rule = QueueFilterRule::deserialize(
                            serde::de::value::MapAccessDeserializer::new(&mut replay),
                        )?;
                        Ok(QueueFilterRuleExp::Single(rule))
                    }
                    None => Err(serde::de::Error::custom("expected a non-empty map")),
                }
            }
        }

        deserializer.deserialize_map(ExpVisitor)
    }
}

impl JsonSchema for QueueFilterRuleExp {
    fn schema_name() -> String {
        "QueueFilterRuleExp".to_string()
    }

    fn json_schema(r#gen: &mut schemars::r#gen::SchemaGenerator) -> Schema {
        let array_of_objects_schema = Some(Box::new(ArrayValidation {
            items: Some(SingleOrVec::Single(Box::new(Schema::Object(
                SchemaObject {
                    // K8s does not allow definition $ref and require non-empty item value by
                    // default. We need this extension to bypass the check.
                    extensions: [(
                        "x-kubernetes-preserve-unknown-fields".to_string(),
                        serde_json::Value::Bool(true),
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
            )))),
            ..Default::default()
        }));
        let all_of_schema = {
            let mut obj = ObjectValidation::default();
            obj.properties.insert(
                "allOf".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(InstanceType::Array.into()),
                    array: array_of_objects_schema.clone(),
                    ..Default::default()
                }),
            );
            obj.required.insert("allOf".to_string());
            Schema::Object(SchemaObject {
                instance_type: Some(InstanceType::Object.into()),
                object: Some(Box::new(obj)),
                ..Default::default()
            })
        };

        // anyOf variant: { "anyOf": [{}, {}, ...] }
        let any_of_schema = {
            let mut obj = ObjectValidation::default();
            obj.properties.insert(
                "anyOf".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(InstanceType::Array.into()),
                    array: array_of_objects_schema,
                    ..Default::default()
                }),
            );
            obj.required.insert("anyOf".to_string());
            Schema::Object(SchemaObject {
                instance_type: Some(InstanceType::Object.into()),
                object: Some(Box::new(obj)),
                ..Default::default()
            })
        };

        let single_schema = r#gen.subschema_for::<QueueFilterRule>();

        Schema::Object(SchemaObject {
            subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                one_of: Some(vec![all_of_schema, any_of_schema, single_schema]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

/// A single filter rule.
/// When `value` is none, a filter with the specified `key` is required.
/// When `value` is specified, the filter value under `key` must match the rule in `value`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueFilterRule {
    /// Key of the filter.
    pub key: String,
    /// Regex that's used to match against the filter value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[test]
fn check_one_api_group() {
    use kube::Resource;

    assert_eq!(MirrordPolicy::group(&()), MirrordClusterPolicy::group(&()),)
}

#[cfg(test)]
mod test {
    use super::SplitQueuesPolicy;

    #[test]
    fn test_queue_filter_policy_serde_roundtrip() {
        let simple_require_filter_yaml = "requireFilter: true\n";
        let value: SplitQueuesPolicy = serde_yaml::from_str(simple_require_filter_yaml).unwrap();
        assert_eq!(
            simple_require_filter_yaml,
            serde_yaml::to_string(&value).unwrap()
        );

        let simple_all_of_yaml = r#"requireFilter: true
filters:
- queueId: ^test-queue$
  queueType: SQS
  filterRules:
    allOf:
    - key: foo
      value: bar
    - key: baz
"#;
        let value: SplitQueuesPolicy = serde_yaml::from_str(simple_all_of_yaml).unwrap();
        assert_eq!(simple_all_of_yaml, serde_yaml::to_string(&value).unwrap());

        let simple_any_of_yaml = r#"requireFilter: true
filters:
- queueId: ^test-queue$
  queueType: Kafka
  filterRules:
    anyOf:
    - key: foo
      value: bar
    - key: baz
"#;
        let value: SplitQueuesPolicy = serde_yaml::from_str(simple_any_of_yaml).unwrap();
        assert_eq!(simple_any_of_yaml, serde_yaml::to_string(&value).unwrap());

        let simple_one_rule_yaml = r#"requireFilter: true
filters:
- queueId: ^test-queue$
  queueType: Kafka
  filterRules:
    key: foo
    value: bar
"#;
        let value: SplitQueuesPolicy = serde_yaml::from_str(simple_one_rule_yaml).unwrap();
        assert_eq!(simple_one_rule_yaml, serde_yaml::to_string(&value).unwrap());

        let nested_rules_yaml = r#"requireFilter: false
filters:
- queueId: ^test-queue$
  queueType: Kafka
  filterRules:
    anyOf:
    - anyOf:
      - allOf:
        - key: nest-us
          value: ok
        - key: nessst-us-now
      - anyOf:
        - key: did-you-nest-us
          value: yes-i-did
        - key: thank-you
    - allOf:
      - anyOf:
        - key: are-you-sure-you-nested-us
          value: yeah-why
      - allOf:
        - anyOf:
          - allOf:
            - anyOf:
              - key: because-i-dont-feel-nested
                value: how-about-now
              - key: we-are-properly-nested
"#;
        let value: SplitQueuesPolicy = serde_yaml::from_str(nested_rules_yaml).unwrap();
        assert_eq!(nested_rules_yaml, serde_yaml::to_string(&value).unwrap());
    }
}
