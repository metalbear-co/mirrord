use mirrord_config::plan::PLAN_ANNOTATION;
use serde_json::json;

use super::*;

fn explain_path(format: ConfigFormat, path: &str) -> ExplainConfigOptionOutput {
    explain_config_option(ExplainConfigOptionArgs {
        path: path.to_owned(),
        format: Some(format),
    })
    .unwrap()
}

#[test]
fn enum_option() {
    let output = explain_path(ConfigFormat::MirrordJson, "feature.network.incoming.mode");
    assert!(output.found);
    assert!(output.description.is_some());
    assert_eq!(output.types, Some(vec![JsonType::String]));
    assert_eq!(
        output.allowed_values,
        Some(vec![json!("mirror"), json!("steal"), json!("off")])
    );
    assert_eq!(output.plan, Some(OptionPlan::Oss));
    assert!(output.plan_by_alternative.is_none());
    assert!(output.suggestions.is_none());
}

/// Reached through a toggleable `http_filter`, which is a bool or a `$ref`'d object.
#[test]
fn nested_option() {
    let output = explain_path(
        ConfigFormat::MirrordJson,
        "feature.network.incoming.http_filter.header_filter",
    );
    assert!(output.found);
    assert!(output.description.unwrap().contains("header"));
    assert_eq!(output.types, Some(vec![JsonType::String]));
    assert!(output.allowed_values.is_none());
}

#[test]
fn toggleable_option() {
    let output = explain_path(ConfigFormat::MirrordJson, "feature.network.incoming");
    let types = output.types.unwrap();
    assert!(types.contains(&JsonType::String), "{types:?}");
    assert!(types.contains(&JsonType::Object), "{types:?}");
}

#[test]
fn default_value() {
    let output = explain_path(
        ConfigFormat::MirrordJson,
        "feature.network.incoming.tls_delivery.protocol",
    );
    assert_eq!(output.default, Some(json!("tls")));
}

/// Each kind of database branch has a `connection` and a `copy`, documented for that kind: one
/// kind's docs or default would be wrong for the others.
#[test]
fn option_of_several_alternatives() {
    let output = explain_path(
        ConfigFormat::MirrordJson,
        "feature.db_branches.connection.url",
    );
    assert!(output.description.is_none(), "{:?}", output.description);
    let output = explain_path(ConfigFormat::MirrordJson, "feature.db_branches.copy");
    assert!(output.description.is_none(), "{:?}", output.description);
    assert!(output.default.is_none(), "{:?}", output.default);
}

#[test]
fn map_entry() {
    let output = explain_path(ConfigFormat::MirrordJson, "feature.env.override.LOG_LEVEL");
    assert!(output.found);
    assert_eq!(output.types, Some(vec![JsonType::String]));
}

#[test]
fn typo() {
    let output = explain_path(ConfigFormat::MirrordJson, "feature.network.incomng.mode");
    assert!(output.found.not());
    assert!(output.description.is_none());
    assert_eq!(
        output.suggestions.unwrap().first().map(String::as_str),
        Some("feature.network.incoming.mode")
    );
}

#[test]
fn partial_path() {
    let output = explain_path(ConfigFormat::MirrordJson, "incoming.mode");
    assert!(output.found.not());
    assert_eq!(
        output.suggestions.unwrap().first().map(String::as_str),
        Some("feature.network.incoming.mode")
    );
}

#[test]
fn empty_path() {
    let result = explain_config_option(ExplainConfigOptionArgs {
        path: " ".to_owned(),
        format: None,
    });
    assert!(matches!(result, Err(ExplainConfigOptionError::EmptyPath)));
}

#[test]
fn up_option_mapped_onto_mirrord_json() {
    let output = explain_path(
        ConfigFormat::MirrordUpYaml,
        "services.api.http_filter.header_filter",
    );
    assert!(output.found);
    assert!(output.description.is_some());
    assert_eq!(
        output.mirrord_json_path.as_deref(),
        Some("feature.network.incoming.http_filter.header_filter")
    );
}

#[test]
fn up_mode() {
    let output = explain_path(ConfigFormat::MirrordUpYaml, "services.api.default_mode");
    assert_eq!(
        output.allowed_values,
        Some(vec![json!("split"), json!("replace"), json!("mirror")])
    );
    assert_eq!(
        output.mirrord_json_path.as_deref(),
        Some("feature.network.incoming.mode")
    );
}

#[test]
fn up_config_patch() {
    let output = explain_path(
        ConfigFormat::MirrordUpYaml,
        "services.api.config_patch.feature.fs.mode",
    );
    assert!(output.found);
    assert!(output.allowed_values.is_some());
    assert_eq!(output.mirrord_json_path.as_deref(), Some("feature.fs.mode"));
}

/// `none` is a bare `const`, without a `type`.
#[test]
fn up_target() {
    let output = explain_path(ConfigFormat::MirrordUpYaml, "services.api.target");
    let types = output.types.unwrap();
    assert!(types.contains(&JsonType::String), "{types:?}");
    assert!(types.contains(&JsonType::Object), "{types:?}");
    assert_eq!(output.allowed_values, Some(vec![json!("none")]));
}

/// `target`'s own description rather than that of one of its kinds, and no allowed values:
/// `targetless` is one of many target strings.
#[test]
fn target() {
    let output = explain_path(ConfigFormat::MirrordJson, "target");
    let description = output.description.unwrap();
    assert!(
        description.starts_with("The Kubernetes workload"),
        "{description}"
    );
    assert!(output.allowed_values.is_none());
}

/// A `mirrord.json` option asked in a `mirrord-up.yaml` suggests the up setting that maps onto
/// it, then setting it through `config_patch`. A map key is only filled in from the asked path
/// where the paths agree up to it, so `feature.network.…` is not under a service called
/// `network`.
#[test]
fn mirrord_json_path_in_up() {
    let output = explain_path(ConfigFormat::MirrordUpYaml, "feature.network.incoming.mode");
    assert!(output.found.not());
    let suggestions = output.suggestions.unwrap();
    assert_eq!(
        suggestions.get(..2),
        Some(
            [
                "services.<name>.default_mode".to_owned(),
                "services.<name>.config_patch.feature.network.incoming.mode".to_owned(),
            ]
            .as_slice()
        )
    );
    assert!(
        suggestions
            .iter()
            .all(|path| path.starts_with("services.<name>.")),
        "{suggestions:?}"
    );
}

/// A service's `target` holds a `path`, so `target.deployment` exists only in `mirrord.json`.
#[test]
fn up_path_only_in_mirrord_json() {
    let output = explain_path(
        ConfigFormat::MirrordUpYaml,
        "services.api.target.deployment",
    );
    assert!(output.found.not());
    let suggestions = output.suggestions.unwrap();
    assert!(
        suggestions.contains(&"services.api.target.path".to_owned()),
        "{suggestions:?}"
    );
}

#[test]
fn up_only_option() {
    let output = explain_path(ConfigFormat::MirrordUpYaml, "services.api.run.command");
    assert!(output.found);
    assert!(output.description.is_some());
    assert!(output.mirrord_json_path.is_none());
}

#[test]
fn up_typo() {
    let output = explain_path(ConfigFormat::MirrordUpYaml, "services.api.htp_filter");
    assert_eq!(
        output.suggestions.unwrap().first().map(String::as_str),
        Some("services.api.http_filter")
    );
}

#[test]
fn plans() {
    for (format, path, plan) in [
        (
            ConfigFormat::MirrordJson,
            "feature.fs.mode",
            OptionPlan::Oss,
        ),
        (
            ConfigFormat::MirrordJson,
            "feature.split_queues",
            OptionPlan::Team,
        ),
        (
            ConfigFormat::MirrordJson,
            "feature.copy_target.scale_down",
            OptionPlan::Team,
        ),
        (
            ConfigFormat::MirrordJson,
            "feature.network.incoming.tls_delivery.protocol",
            OptionPlan::Team,
        ),
        (
            ConfigFormat::MirrordJson,
            "feature.preview",
            OptionPlan::Enterprise,
        ),
        (
            ConfigFormat::MirrordJson,
            "target.path.deployment",
            OptionPlan::Oss,
        ),
        (
            ConfigFormat::MirrordJson,
            "target.path.job",
            OptionPlan::Team,
        ),
        (ConfigFormat::MirrordJson, "agent.ttl", OptionPlan::Oss),
        (ConfigFormat::MirrordJson, "profile", OptionPlan::Team),
        (ConfigFormat::MirrordJson, "traceparent", OptionPlan::Team),
        (ConfigFormat::MirrordJson, "baggage", OptionPlan::Team),
        (
            ConfigFormat::MirrordJson,
            "multi_cluster",
            OptionPlan::Enterprise,
        ),
        (
            ConfigFormat::MirrordUpYaml,
            "services.api.config_patch.feature.db_branches",
            OptionPlan::Team,
        ),
        (
            ConfigFormat::MirrordUpYaml,
            "services.api.target.path.stateful_set",
            OptionPlan::Team,
        ),
    ] {
        assert_eq!(explain_path(format, path).plan, Some(plan), "{path}");
    }
}

/// `target.path` can be used for free, but some kinds of targets need a paid plan.
#[test]
fn target_kinds() {
    let output = explain_path(ConfigFormat::MirrordJson, "target.path");
    assert_eq!(output.plan, Some(OptionPlan::Oss));

    let by_alternative = output.plan_by_alternative.unwrap();
    for (alternative, plan) in [
        ("deployment", Plan::Oss),
        ("pod", Plan::Oss),
        ("rollout", Plan::Oss),
        ("job", Plan::Team),
        ("labels", Plan::Team),
        ("serverless", Plan::Enterprise),
    ] {
        assert!(
            by_alternative.contains(&AlternativePlan {
                alternative: alternative.to_owned(),
                plan,
            }),
            "{alternative}: {by_alternative:?}"
        );
    }
}

#[test]
fn up_mode_plans() {
    let output = explain_path(ConfigFormat::MirrordUpYaml, "services.api.default_mode");
    assert_eq!(output.plan, Some(OptionPlan::Oss));
    assert_eq!(
        output.plan_by_alternative,
        Some(vec![
            AlternativePlan {
                alternative: "split".to_owned(),
                plan: Plan::Oss,
            },
            AlternativePlan {
                alternative: "replace".to_owned(),
                plan: Plan::Team,
            },
            AlternativePlan {
                alternative: "mirror".to_owned(),
                plan: Plan::Oss,
            },
        ])
    );
}

/// Every option needs a plan, its own or its parent's: `#[config(plan = Team)]` on fields
/// generated by `MirrordConfig`, `#[schemars(extend("x-mirrord-plan" = Plan::Team))]`
/// elsewhere.
#[test]
fn every_option_has_a_plan() {
    let missing: Vec<&String> = LAYER_PATHS
        .iter()
        .filter(|path| {
            explain_path(ConfigFormat::MirrordJson, path).plan == Some(OptionPlan::Unverified)
        })
        .collect();
    assert!(missing.is_empty(), "options without a plan: {missing:#?}");
}

/// Every option under `feature` needs a plan of its own rather than inheriting `feature`'s, so
/// a new feature can't be added without deciding which plan it needs.
#[test]
fn every_feature_has_its_own_plan() {
    let features = LAYER_SCHEMA
        .raw
        .pointer("/$defs/FeatureFileConfig/properties")
        .and_then(Value::as_object)
        .unwrap();
    let missing: Vec<&String> = features
        .iter()
        .filter(|(_, schema)| {
            schema
                .get(PLAN_ANNOTATION)
                .and_then(|plan| Plan::deserialize(plan).ok())
                .is_none()
        })
        .map(|(name, _)| name)
        .collect();
    assert!(missing.is_empty(), "features without a plan: {missing:#?}");
}

/// Every target kind needs a plan of its own, rather than inheriting `target`'s.
#[test]
fn every_target_kind_has_a_plan() {
    let kinds = LAYER_SCHEMA
        .raw
        .pointer("/$defs/Target/oneOf")
        .and_then(Value::as_array)
        .unwrap();
    for kind in kinds {
        let kind = match kind.get("$ref").and_then(Value::as_str) {
            Some(reference) => LAYER_SCHEMA.raw.pointer(&reference[1..]).unwrap(),
            None => kind,
        };
        let plan = kind.get(PLAN_ANNOTATION).cloned();
        assert!(
            plan.and_then(|plan| Plan::deserialize(plan).ok()).is_some(),
            "target kind without a valid plan: {kind}"
        );
    }
}

#[test]
fn every_up_option_has_a_plan() {
    let missing: Vec<&String> = UP_PATHS
        .iter()
        .filter(|path| {
            explain_path(ConfigFormat::MirrordUpYaml, path).plan == Some(OptionPlan::Unverified)
        })
        .collect();
    assert!(missing.is_empty(), "options without a plan: {missing:#?}");
}
