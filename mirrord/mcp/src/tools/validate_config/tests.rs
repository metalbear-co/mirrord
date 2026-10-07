use rstest::rstest;
use serde_json::json;

use super::*;

fn validate(format: ConfigFormat, content: &str) -> ValidateConfigOutput {
    validate_config(ValidateConfigArgs {
        format,
        content: content.to_owned(),
        key: None,
    })
    .unwrap()
}

fn single_issue(format: ConfigFormat, content: &str) -> ConfigIssue {
    let mut output = validate(format, content);
    assert!(output.valid.not());
    assert_eq!(output.issues.len(), 1, "{:?}", output.issues);
    output.issues.remove(0)
}

#[test]
fn schemas_compile() {
    assert!(LAYER_SCHEMA.validator().is_ok());
    assert!(UP_SCHEMA.validator().is_ok());
}

#[test]
fn valid_mirrord_json() {
    let output = validate(
        ConfigFormat::MirrordJson,
        r#"{
                "target": { "path": "deployment/app", "namespace": "default" },
                "feature": {
                    "network": { "incoming": { "mode": "steal" } },
                    "fs": "read"
                }
            }"#,
    );
    assert!(output.valid, "{:?}", output.issues);
    assert!(output.issues.is_empty());
}

#[test]
fn templated_mirrord_json() {
    let output = validate(
        ConfigFormat::MirrordJson,
        r#"{ "key": "{{ git_branch | default(value='main') }}", "feature": { "network": { "incoming": { "mode": "steal", "http_filter": { "header_filter": "x-session: {{ key }}" } } } } }"#,
    );
    assert!(output.valid, "{:?}", output.issues);
}

#[test]
fn unknown_field() {
    let issue = single_issue(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "network": { "incomin": {} } } }"#,
    );
    assert_eq!(issue.path, "/feature/network/incomin");
    let allowed = issue.allowed_values.unwrap();
    assert!(allowed.contains(&json!("incoming")), "{allowed:?}");
    assert!(allowed.contains(&json!("outgoing")), "{allowed:?}");
}

#[test]
fn bad_enum_value() {
    let issue = single_issue(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "network": { "incoming": { "mode": "foo" } } } }"#,
    );
    assert_eq!(issue.path, "/feature/network/incoming/mode");
    let allowed = issue.allowed_values.unwrap();
    assert!(allowed.contains(&json!("steal")), "{allowed:?}");
    assert!(allowed.contains(&json!("mirror")), "{allowed:?}");
}

/// `connection` is a serde alias of `source`, which the schema doesn't know about.
#[test]
fn serde_alias() {
    let output = validate(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "db_branches": [ { "type": "turbopuffer", "connection": { "params": {
                "namespace": "TPUF_NAMESPACE",
                "api_key": "TURBOPUFFER_API_KEY",
                "region": { "env_var_name": "TURBOPUFFER_REGION", "value": "gcp-us-central1" }
            } } } ] } }"#,
    );
    assert!(output.valid, "{:?}", output.issues);
}

/// Passes the schema and deserializes, but `verify` rejects it.
#[test]
fn conflicting_http_filters() {
    let issue = single_issue(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "network": { "incoming": { "mode": "steal",
                "http_filter": { "header_filter": "a", "path_filter": "b" } } } } }"#,
    );
    assert!(
        issue.message.contains("multiple types of HTTP filter"),
        "{}",
        issue.message
    );
}

/// A missing target may still come from the command line, so it doesn't make the config
/// targetless (which would conflict with `steal`).
#[test]
fn missing_target_not_final() {
    let output = validate(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "network": { "incoming": "steal" } } }"#,
    );
    assert!(output.valid, "{:?}", output.issues);
}

#[test]
fn up_yaml_key_dependent_template() {
    let content = r#"
services:
  app:
    run:
      command: ["true"]
{% if key == "prod" %}    bogus: 1
{% endif %}"#;
    assert!(validate(ConfigFormat::MirrordUpYaml, content).valid);

    let mut output = validate_config(ValidateConfigArgs {
        format: ConfigFormat::MirrordUpYaml,
        content: content.to_owned(),
        key: Some("prod".to_owned()),
    })
    .unwrap();
    let issue = output.issues.pop().unwrap();
    assert!(output.issues.is_empty(), "{:?}", output.issues);
    assert_eq!(issue.path, "/services/app/bogus");
}

#[test]
fn json_syntax_error() {
    let issue = single_issue(ConfigFormat::MirrordJson, r#"{ "feature": "#);
    assert_eq!(issue.path, "");
    assert!(issue.message.contains("line"), "{}", issue.message);
}

#[test]
fn valid_up_yaml() {
    let output = validate(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  consumer:
    target:
      path: deployment/consumer
    run:
      command: ["python", "-m", "http.server", "{{ key }}"]
"#,
    );
    assert!(output.valid, "{:?}", output.issues);
}

#[test]
fn up_yaml_unknown_field() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  consumer:
    run:
      command: ["python"]
    bogus: true
"#,
    );
    assert_eq!(issue.path, "/services/consumer/bogus");
    let allowed = issue.allowed_values.unwrap();
    assert!(allowed.contains(&json!("run")), "{allowed:?}");
}

#[test]
fn up_yaml_valid_config_patch() {
    let output = validate(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  worker:
    config_patch:
      feature:
        split_queues:
          "*":
            queue_type: SQS
            jq_filter: '.Body | fromjson | .headers["x-origin"] == "{{ key }}"'
    run:
      command: ["echo"]
"#,
    );
    assert!(output.valid, "{:?}", output.issues);
}

#[test]
fn up_yaml_invalid_config_patch() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  worker:
    config_patch:
      feature:
        split_queues: NOT_A_SPLIT_QUEUE_CONFIG
    run:
      command: ["echo"]
"#,
    );
    assert_eq!(
        issue.path,
        "/services/worker/config_patch/feature/split_queues"
    );
}

/// Deserializes, but `verify` rejects the jq filter.
#[test]
fn up_yaml_config_patch_invalid_jq_filter() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  worker:
    config_patch:
      feature:
        split_queues:
          "*":
            queue_type: SQS
            jq_filter: "["
    run:
      command: ["echo"]
"#,
    );
    assert_eq!(issue.path, "/services/worker/config_patch");
    assert!(issue.message.contains("jq"), "{}", issue.message);
}

/// A patch is merged into the config `mirrord up` generates, not only into the service's own
/// settings, so it conflicts with what `mirrord up` sets by itself.
#[rstest]
#[case::default_header_filter(
    "config_patch: { feature: { network: { incoming: { http_filter: { path_filter: /api } } } } }",
    "/services/api/config_patch/feature/network/incoming/http_filter",
    "multiple types of HTTP filter"
)]
#[case::common_operator(
    "config_patch: { feature: { copy_target: true } }",
    "/services/api/config_patch/feature/copy_target",
    "requires a mirrord operator"
)]
fn up_yaml_config_patch_conflicts_with_generated(
    #[case] service: &str,
    #[case] path: &str,
    #[case] message: &str,
) {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        &format!(
            "common:\n  operator: false\nservices:\n  api:\n    {service}\n    run:\n      command: [x]\n"
        ),
    );
    assert_eq!(issue.path, path);
    assert!(issue.message.contains(message), "{}", issue.message);
}

#[test]
fn up_yaml_config_patch_unknown_field() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  worker:
    config_patch:
      feature:
        netwrk: {}
    run:
      command: ["echo"]
"#,
    );
    assert_eq!(issue.path, "/services/worker/config_patch/feature/netwrk");
    assert!(issue.allowed_values.unwrap().contains(&json!("network")));
}

/// A service's issue isn't mistaken for a duplicate of the patch issue of another service
/// whose name it prefixes.
#[test]
fn up_yaml_patch_issue_of_prefixed_service() {
    let output = validate(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  app:
    target: none
    run:
      command: ["echo"]
  app-v2:
    config_patch:
      feature:
        netwrk: {}
    run:
      command: ["echo"]
"#,
    );
    let paths: Vec<_> = output.issues.iter().map(|issue| &issue.path).collect();
    assert_eq!(
        paths,
        [
            "/services/app",
            "/services/app-v2/config_patch/feature/netwrk"
        ],
    );
}

/// Deserializes, but `mirrord up` only supports `run.directory` for `exec` services.
#[test]
fn up_yaml_container_run_directory() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        r#"
services:
  app:
    run:
      type: container
      directory: ./app
      command: ["docker", "run", "app"]
"#,
    );
    assert_eq!(issue.path, "/services/app/run/directory");
    assert!(issue.message.contains("type: exec"), "{}", issue.message);
}

/// `target` is `none` or a mapping, so a string other than `none` is reported along with the
/// mapping it could have been.
#[test]
fn up_yaml_string_target() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    target: deployment/api\n    run:\n      command: [\"true\"]\n",
    );
    assert_eq!(issue.path, "/services/api/target");
    assert_eq!(issue.allowed_values, Some(vec![json!("none")]));
    assert!(issue.message.contains("`object`"), "{}", issue.message);
}

/// A repeated field is reported at the field itself.
#[rstest]
#[case::top_level(
    r#"{ "target": "deployment/a", "target": "pod/b" }"#,
    "/target",
    "target"
)]
#[case::nested(
    r#"{ "feature": { "network": { "incoming": { "port_mapping": [[1, 2]], "port_mapping": [[3, 4]] } } } }"#,
    "/feature/network/incoming/port_mapping",
    "port_mapping"
)]
fn duplicate_field(#[case] content: &str, #[case] path: &str, #[case] field: &str) {
    let issue = single_issue(ConfigFormat::MirrordJson, content);
    assert_eq!(issue.path, path);
    assert!(
        issue
            .message
            .contains(&format!("duplicate field `{field}`")),
        "{}",
        issue.message
    );
}

/// A repeated field is found inside an option with several forms too, and next to other
/// issues.
#[test]
fn duplicate_field_next_to_other_issues() {
    let output = validate(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "fs": { "mode": "read", "mode": "write" }, "network": { "incoming": { "mode": "foo" } } } }"#,
    );
    let paths: Vec<&str> = output
        .issues
        .iter()
        .map(|issue| issue.path.as_str())
        .collect();
    assert!(paths.contains(&"/feature/fs/mode"), "{:?}", output.issues);
    assert!(
        paths.contains(&"/feature/network/incoming/mode"),
        "{:?}",
        output.issues
    );
}

/// A key given twice in a `mirrord-up.yaml` is rejected, as `mirrord up` rejects it, with a
/// message for the author of the file rather than for the program parsing it.
#[test]
fn up_yaml_duplicate_key() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    target: none\n    target: none\n    run:\n      command: [\"true\"]\n",
    );
    assert!(
        issue.message.contains("duplicate mapping key: target"),
        "{}",
        issue.message
    );
    assert!(issue.message.contains("line 4"), "{}", issue.message);
    assert_eq!(issue.path, "/services/api/target");
    assert!(
        issue.message.contains("DuplicateKeyPolicy").not(),
        "{}",
        issue.message
    );
}

#[rstest]
#[case::mirrord_json(
    ConfigFormat::MirrordJson,
    r#"{ "feature": { "network": { "incoming": { "http_filter": { "header_filter": "([" } } } } }"#,
    "/feature/network/incoming/http_filter/header_filter"
)]
#[case::up_yaml(
    ConfigFormat::MirrordUpYaml,
    "services:\n  api:\n    http_filter:\n      header_filter: \"([\"\n    run:\n      command: [\"true\"]\n",
    "/services/api/http_filter/header_filter"
)]
fn invalid_http_filter_regex(
    #[case] format: ConfigFormat,
    #[case] content: &str,
    #[case] path: &str,
) {
    let issue = single_issue(format, content);
    assert_eq!(issue.path, path);
    assert!(issue.message.contains("`([`"), "{}", issue.message);
}

/// Both formats offer `target` as a string or a mapping, so the unknown field has to be found
/// in the mapping.
#[rstest]
#[case::mirrord_json(
    ConfigFormat::MirrordJson,
    r#"{ "target": { "path": "deployment/api", "bogus": 1 } }"#,
    "/target/bogus"
)]
#[case::up_yaml(
    ConfigFormat::MirrordUpYaml,
    "services:\n  api:\n    target:\n      path: deployment/api\n      bogus: 1\n    run:\n      command: [\"true\"]\n",
    "/services/api/target/bogus"
)]
fn unknown_target_field(#[case] format: ConfigFormat, #[case] content: &str, #[case] path: &str) {
    let issue = single_issue(format, content);
    assert_eq!(issue.path, path);
    assert_eq!(issue.message, "unknown field `bogus`");
}

/// Conflicts point at the setting to change.
#[rstest]
#[case::http_filters(
    ConfigFormat::MirrordJson,
    r#"{ "feature": { "network": { "incoming": { "mode": "steal", "http_filter": { "header_filter": "a", "path_filter": "b" } } } } }"#,
    "/feature/network/incoming/http_filter"
)]
#[case::env_include_exclude(
    ConfigFormat::MirrordJson,
    r#"{ "feature": { "env": { "include": "A", "exclude": "B" } } }"#,
    "/feature/env"
)]
#[case::copy_target_targetless(
    ConfigFormat::MirrordJson,
    r#"{ "target": "targetless", "feature": { "copy_target": true } }"#,
    "/feature/copy_target"
)]
#[case::config_patch(
    ConfigFormat::MirrordUpYaml,
    "services:\n  api:\n    config_patch:\n      feature:\n        env:\n          include: A\n          exclude: B\n    run:\n      command: [\"true\"]\n",
    "/services/api/config_patch/feature/env"
)]
#[case::service_setting(
    ConfigFormat::MirrordUpYaml,
    "services:\n  api:\n    env:\n      include: A\n      exclude: B\n    run:\n      command: [\"true\"]\n",
    "/services/api/env"
)]
fn conflict_location(#[case] format: ConfigFormat, #[case] content: &str, #[case] path: &str) {
    let issue = single_issue(format, content);
    assert_eq!(issue.path, path);
    assert!(
        issue.message.starts_with("Conflicting configuration"),
        "{}",
        issue.message
    );
}

/// The reason a target path doesn't parse, rather than serde's "did not match any variant".
#[rstest]
#[case::simple(r#"{ "target": "banana/api" }"#, "/target")]
#[case::advanced(
    r#"{ "target": { "path": "banana/api", "namespace": "default" } }"#,
    "/target/path"
)]
fn invalid_target_path(#[case] content: &str, #[case] path: &str) {
    let issue = single_issue(ConfigFormat::MirrordJson, content);
    assert_eq!(issue.path, path);
    assert_eq!(issue.message, "`banana/api` is not a valid target path");
    let formats = issue.allowed_values.unwrap();
    assert!(
        formats.contains(&json!(
            "deploy/{deployment-name}[/container/{container-name}]"
        )),
        "{formats:?}"
    );
    assert!(
        formats.contains(&json!(
            "deployment/{deployment-name}[/container/{container-name}]"
        )),
        "{formats:?}"
    );
}

/// A specific reason is kept, a malformed label target here.
#[test]
fn invalid_label_target() {
    let issue = single_issue(ConfigFormat::MirrordJson, r#"{ "target": "label/app" }"#);
    assert_eq!(issue.path, "/target");
    assert!(
        issue
            .message
            .starts_with("`label/app` is not a valid target path: Label target"),
        "{}",
        issue.message
    );
}

/// The duplicate key is found wherever and however it's written.
#[rstest]
#[case::flow(
    "services: {api: {target: none, target: none, run: {command: [x]}}}\n",
    "/services/api/target"
)]
#[case::in_config_patch(
    "services:\n  api:\n    config_patch:\n      feature:\n        env:\n          override:\n            A: x\n            A: y\n    run:\n      command: [x]\n",
    "/services/api/config_patch/feature/env/override/A"
)]
#[case::in_sequence(
    "services:\n  api:\n    run:\n      command: [{a: 1, a: 2}]\n",
    "/services/api/run/command/0/a"
)]
fn up_yaml_duplicate_key_path(#[case] content: &str, #[case] path: &str) {
    assert_eq!(
        single_issue(ConfigFormat::MirrordUpYaml, content).path,
        path
    );
}

/// Errors that name the setting they're about point at it. An index-less `[]` in the name
/// (meaning "any entry") points at the list.
#[rstest]
#[case::field(
    r#"{ "startup_retry": { "min_ms": 10, "max_ms": 5 } }"#,
    "/startup_retry/min_ms"
)]
#[case::leading_dot(
    r#"{ "feature": { "network": { "incoming": { "mode": "steal", "tls_delivery": { "protocol": "tls", "server_name": "not a name!" } } } } }"#,
    "/feature/network/incoming/tls_delivery/server_name"
)]
#[case::indexed(
    r#"{ "feature": { "preview": { "config_mounts": [{ "payload": "!!!", "type": "binary", "mount_at": "/x" }] } } }"#,
    "/feature/preview/config_mounts/0/payload"
)]
#[case::any_entry(
    r#"{ "feature": { "db_branches": [{ "type": "redis", "name": "abc", "connection": { "url": { "type": "env", "variable": "X" } } }] } }"#,
    "/feature/db_branches"
)]
fn config_error_location(#[case] content: &str, #[case] path: &str) {
    assert_eq!(single_issue(ConfigFormat::MirrordJson, content).path, path);
}

/// `mirrord up` ignores the `http_filter` of a service in `replace` mode.
#[test]
fn up_yaml_replace_mode_ignores_http_filter() {
    let output = validate(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    default_mode: replace\n    http_filter:\n      header_filter: \"(\"\n    run:\n      command: [\"true\"]\n",
    );
    assert!(output.valid, "{:?}", output.issues);
}

#[test]
fn accepted_forms() {
    let issue = single_issue(ConfigFormat::MirrordJson, r#"{ "agent": { "image": 5 } }"#);
    assert_eq!(issue.path, "/agent/image");
    assert!(issue.message.contains("a `string`"), "{}", issue.message);
    assert!(issue.message.contains("`registry`"), "{}", issue.message);
}

#[test]
fn top_level_array() {
    let issue = single_issue(ConfigFormat::MirrordJson, "[]");
    assert_eq!(issue.path, "");
}

/// A database branch with a typo is reported for the typo, not as the one kind of branch that
/// takes any field (Redis), whose `type` it doesn't have.
#[rstest]
#[case::missing_field(
    r#"{ "feature": { "db_branches": [{ "type": "pg", "conection": { "url": { "type": "env", "variable": "X" } } }] } }"#,
    "/feature/db_branches/0/conection"
)]
#[case::complete(
    r#"{ "feature": { "db_branches": [{ "type": "pg", "version": "16", "connection": { "url": { "type": "env", "variable": "X" } }, "ttl_sec": 5 }] } }"#,
    "/feature/db_branches/0/ttl_sec"
)]
fn tagged_alternative_typo(#[case] content: &str, #[case] path: &str) {
    let output = validate(ConfigFormat::MirrordJson, content);
    assert!(
        output
            .issues
            .iter()
            .any(|issue| issue.path == path && issue.message.starts_with("unknown field")),
        "{:?}",
        output.issues
    );
    assert!(
        output
            .issues
            .iter()
            .all(|issue| issue.path.ends_with("/type").not()),
        "{:?}",
        output.issues
    );
}

/// A service's settings and its `config_patch` are checked together, as `mirrord up` checks
/// the generated config with the patch merged over it.
#[rstest]
#[case::patch_replaces_setting(
    "services:\n  api:\n    http_filter:\n      header_filter: \"(\"\n    config_patch:\n      feature:\n        network:\n          incoming:\n            http_filter:\n              header_filter: ok\n    run:\n      command: [\"true\"]\n",
    None
)]
#[case::patch_conflicts_with_setting(
    "services:\n  api:\n    ignore_ports: [80]\n    config_patch:\n      feature:\n        network:\n          incoming:\n            ports: [81]\n    run:\n      command: [\"true\"]\n",
    Some("/services/api/config_patch/feature/network/incoming/ports")
)]
fn up_yaml_settings_with_config_patch(#[case] content: &str, #[case] path: Option<&str>) {
    let output = validate(ConfigFormat::MirrordUpYaml, content);
    match path {
        None => assert!(output.valid, "{:?}", output.issues),
        Some(path) => {
            let [issue] = output.issues.as_slice() else {
                panic!("{:?}", output.issues);
            };
            assert_eq!(issue.path, path);
        }
    }
}

/// The settings of a service are checked next to other issues of the file.
#[test]
fn up_yaml_service_settings_next_to_other_issues() {
    let output = validate(
        ConfigFormat::MirrordUpYaml,
        "common:\n  bogus: 1\nservices:\n  api:\n    http_filter:\n      header_filter: \"([\"\n    run:\n      command: [x]\n",
    );
    let paths: Vec<&str> = output
        .issues
        .iter()
        .map(|issue| issue.path.as_str())
        .collect();
    assert!(paths.contains(&"/common/bogus"), "{:?}", output.issues);
    assert!(
        paths.contains(&"/services/api/http_filter/header_filter"),
        "{:?}",
        output.issues
    );
}

#[test]
fn up_yaml_missing_services() {
    let issue = single_issue(ConfigFormat::MirrordUpYaml, "common: {}\n");
    assert_eq!(issue.path, "");
    assert!(issue.message.contains("services"), "{}", issue.message);
}

/// Services `mirrord up` refuses once it assembles their config, though every setting is
/// valid on its own.
#[rstest]
#[case::targetless_split("target: none", "Steal mode")]
#[case::targetless_patched_steal(
    "target: none\n    default_mode: mirror\n    config_patch: { feature: { network: { incoming: steal } } }",
    "Steal mode"
)]
#[case::targetless_replace("target: none\n    default_mode: replace", "targetless agent")]
#[case::targetless_patched_copy(
    "target: none\n    default_mode: mirror\n    config_patch: { feature: { copy_target: true } }",
    "copy target"
)]
#[case::service_patched_copy(
    "target: { path: service/app }\n    config_patch: { feature: { copy_target: true } }",
    "service targets"
)]
#[case::pod_replace("target: { path: pod/app }\n    default_mode: replace", "pod target")]
#[case::rollout_replace(
    "target: { path: rollout/app }\n    default_mode: replace",
    "rollout target"
)]
fn up_yaml_unrunnable_service(#[case] service: &str, #[case] message: &str) {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        &format!("services:\n  app:\n    {service}\n    run:\n      command: [\"true\"]\n"),
    );
    assert_eq!(issue.path, "/services/app");
    assert!(issue.message.contains(message), "{}", issue.message);
}

/// A service that fails to assemble because of one of its settings is reported once, at that
/// setting.
#[test]
fn up_yaml_unrunnable_service_setting() {
    let issue = single_issue(
        ConfigFormat::MirrordUpYaml,
        "services:\n  app:\n    env: { include: [A], exclude: [B] }\n    run:\n      command: [\"true\"]\n",
    );
    assert_eq!(issue.path, "/services/app/env");
    assert!(
        issue.message.contains("`include` and `exclude`"),
        "{}",
        issue.message
    );
}

/// The same settings with a target or mode that supports them.
#[rstest]
#[case::targetless_mirror("target: none\n    default_mode: mirror")]
#[case::deployment_replace("target: { path: deployment/app }\n    default_mode: replace")]
#[case::inferred_target_replace("default_mode: replace")]
fn up_yaml_runnable_service(#[case] service: &str) {
    let output = validate(
        ConfigFormat::MirrordUpYaml,
        &format!("services:\n  app:\n    {service}\n    run:\n      command: [\"true\"]\n"),
    );
    assert!(output.valid, "{:?}", output.issues);
}
