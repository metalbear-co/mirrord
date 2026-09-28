//! Step 3 of the `--resource` pipeline: comparing the user's objects with the live ones.
//!
//! Both `preview start` (to skip what is unchanged) and `preview diff` (to show what changed)
//! use [`diff`]: an object is unchanged exactly when it yields no [`FieldChange`].
//!
//! Only the parts that shape the preview pod are compared, each reduced to a comparable form
//! first: the pod template of the target, `data`/`binaryData` of a ConfigMap, and
//! `type`/`data` of a Secret. That leaves out everything the API server writes on its own
//! (`status`, `metadata.resourceVersion`, `uid`, `managedFields`, ...). Fields the API server
//! fills with defaults are handled by comparing the live template with the user's template
//! as the API server returns it from a dry run (see `super::plan`), so both sides carry the
//! same defaults.

use std::{collections::BTreeMap, fmt};

use base64::prelude::*;
use serde_json::{Map, Value};

/// What changed at one path of an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FieldChange {
    Added {
        path: String,
        value: Value,
    },
    Removed {
        path: String,
        value: Value,
    },
    Changed {
        path: String,
        old: Value,
        new: Value,
    },
}

impl FieldChange {
    pub fn path(&self) -> &str {
        match self {
            Self::Added { path, .. } | Self::Removed { path, .. } | Self::Changed { path, .. } => {
                path
            }
        }
    }
}

/// Every difference between `live` and `supplied`, under `prefix`.
///
/// Lists whose items all carry a `name` (containers, env vars, volumes, ports, ...) are matched
/// by name, the way Kubernetes merges them, so reordering is not a change and the path says
/// which item changed (`containers[app].env[LOG_LEVEL].value`). Other lists are compared as a
/// whole.
pub(crate) fn diff(prefix: &str, live: &Value, supplied: &Value) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    diff_into(prefix, live, supplied, &mut changes);
    changes
}

fn diff_into(path: &str, live: &Value, supplied: &Value, changes: &mut Vec<FieldChange>) {
    match (live, supplied) {
        (Value::Object(live), Value::Object(supplied)) => {
            for (key, live_value) in live {
                let child = join(path, key);
                match supplied.get(key) {
                    Some(supplied_value) => diff_into(&child, live_value, supplied_value, changes),
                    None => changes.push(FieldChange::Removed {
                        path: child,
                        value: live_value.clone(),
                    }),
                }
            }
            for (key, supplied_value) in supplied {
                if !live.contains_key(key) {
                    changes.push(FieldChange::Added {
                        path: join(path, key),
                        value: supplied_value.clone(),
                    });
                }
            }
        }
        (Value::Array(live_items), Value::Array(supplied_items)) => {
            match (by_name(live_items), by_name(supplied_items)) {
                (Some(live_named), Some(supplied_named)) => {
                    let as_object = |named: BTreeMap<&str, &Value>| {
                        Value::Object(
                            named
                                .into_iter()
                                .map(|(name, item)| (name.to_owned(), item.clone()))
                                .collect(),
                        )
                    };
                    diff_named(
                        path,
                        &as_object(live_named),
                        &as_object(supplied_named),
                        changes,
                    );
                }
                _ if live_items != supplied_items => changes.push(FieldChange::Changed {
                    path: path.to_owned(),
                    old: Value::Array(live_items.clone()),
                    new: Value::Array(supplied_items.clone()),
                }),
                _ => {}
            }
        }
        (live, supplied) if live != supplied => changes.push(FieldChange::Changed {
            path: path.to_owned(),
            old: live.clone(),
            new: supplied.clone(),
        }),
        _ => {}
    }
}

/// Same as the object arm of [`diff_into`], with `[name]` path segments for list items.
fn diff_named(path: &str, live: &Value, supplied: &Value, changes: &mut Vec<FieldChange>) {
    let (Value::Object(live), Value::Object(supplied)) = (live, supplied) else {
        return;
    };

    for (name, live_item) in live {
        let child = format!("{path}[{name}]");
        match supplied.get(name) {
            Some(supplied_item) => diff_into(&child, live_item, supplied_item, changes),
            None => changes.push(FieldChange::Removed {
                path: child,
                value: live_item.clone(),
            }),
        }
    }
    for (name, supplied_item) in supplied {
        if !live.contains_key(name) {
            changes.push(FieldChange::Added {
                path: format!("{path}[{name}]"),
                value: supplied_item.clone(),
            });
        }
    }
}

/// The items keyed by their `name`, when every item has a distinct one.
fn by_name(items: &[Value]) -> Option<BTreeMap<&str, &Value>> {
    let mut named = BTreeMap::new();
    for item in items {
        let name = item.get("name")?.as_str()?;
        if named.insert(name, item).is_some() {
            return None;
        }
    }
    Some(named)
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// A pod template reduced to what the preview pod uses: `null` fields dropped (a manifest's
/// `key:` with no value and an absent key mean the same), and of the metadata only the labels
/// and annotations the preview pod inherits. The rest of a template's metadata is either
/// server-written (`creationTimestamp: null`) or, for a Pod target, the Pod's own identity.
pub(crate) fn comparable_template(template: &Value) -> Value {
    let mut template = without_nulls(template);
    let Some(object) = template.as_object_mut() else {
        return template;
    };

    let metadata: Map<String, Value> = object
        .remove("metadata")
        .and_then(|metadata| match metadata {
            Value::Object(metadata) => Some(metadata),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|(key, value)| {
            matches!(key.as_str(), "labels" | "annotations")
                && value.as_object().is_some_and(|entries| !entries.is_empty())
        })
        .collect();
    if !metadata.is_empty() {
        object.insert("metadata".to_owned(), Value::Object(metadata));
    }

    template
}

/// A ConfigMap reduced to its contents. Numbers and booleans in `data` are compared by their
/// text, which is what the API server stores them as.
pub(crate) fn comparable_config_map(config_map: &Value) -> Value {
    let mut comparable = Map::new();
    for field in ["data", "binaryData"] {
        let Some(Value::Object(entries)) = config_map.get(field) else {
            continue;
        };
        let entries: Map<String, Value> = entries
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(scalar_text(value))))
            .collect();
        if !entries.is_empty() {
            comparable.insert(field.to_owned(), Value::Object(entries));
        }
    }
    Value::Object(comparable)
}

/// A Secret's values as bytes: `data` base64-decoded, with `stringData` applied on top, the way
/// the API server merges them when it stores a Secret.
pub(crate) fn secret_bytes(secret: &Value) -> Result<BTreeMap<String, Vec<u8>>, SecretDecodeError> {
    let mut bytes = BTreeMap::new();

    if let Some(Value::Object(data)) = secret.get("data") {
        for (key, value) in data {
            let encoded: String = scalar_text(value)
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            let decoded = BASE64_STANDARD
                .decode(&encoded)
                .map_err(|_| SecretDecodeError { key: key.clone() })?;
            bytes.insert(key.clone(), decoded);
        }
    }

    if let Some(Value::Object(string_data)) = secret.get("stringData") {
        for (key, value) in string_data {
            bytes.insert(key.clone(), scalar_text(value).into_bytes());
        }
    }

    Ok(bytes)
}

/// A `data` value of a Secret manifest that is not valid base64.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SecretDecodeError {
    pub key: String,
}

impl fmt::Display for SecretDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`data.{}` is not valid base64; put plain text under `stringData` instead",
            self.key
        )
    }
}

/// A Secret reduced to its type and values, the values re-encoded as canonical base64 so two
/// encodings of the same bytes compare equal. Values are never printed: see
/// [`super::report`], which only says whether each one changed.
pub(crate) fn comparable_secret(secret: &Value) -> Result<Value, SecretDecodeError> {
    let secret_type = secret
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Opaque");

    let data: Map<String, Value> = secret_bytes(secret)?
        .into_iter()
        .map(|(key, value)| (key, Value::String(BASE64_STANDARD.encode(value))))
        .collect();

    let mut comparable = Map::new();
    comparable.insert("type".to_owned(), Value::String(secret_type.to_owned()));
    if !data.is_empty() {
        comparable.insert("data".to_owned(), Value::Object(data));
    }
    Ok(Value::Object(comparable))
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), without_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests read JSON fixtures with `value[key]`; a panic just fails the test.
mod tests {
    use serde_json::json;

    use super::*;

    fn template(env_value: &str) -> Value {
        json!({
            "metadata": {"labels": {"app": "app"}, "creationTimestamp": null},
            "spec": {"containers": [
                {"name": "sidecar", "image": "proxy"},
                {"name": "app", "image": "app:1", "env": [
                    {"name": "LOG_LEVEL", "value": env_value},
                    {"name": "PORT", "value": "80"},
                ]},
            ]},
        })
    }

    /// The same template, with server-written `null`s and reordered lists, is unchanged.
    #[test]
    fn identical_after_normalization_has_no_changes() {
        let live = comparable_template(&template("info"));
        let mut supplied = template("info");
        supplied["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("creationTimestamp");
        supplied["spec"]["containers"]
            .as_array_mut()
            .unwrap()
            .reverse();
        supplied["spec"]["dnsPolicy"] = Value::Null;

        assert_eq!(
            diff("spec.template", &live, &comparable_template(&supplied)),
            []
        );
    }

    #[test]
    fn changed_env_var_is_reported_by_container_and_variable_name() {
        let changes = diff(
            "spec.template",
            &comparable_template(&template("info")),
            &comparable_template(&template("debug")),
        );

        assert_eq!(
            changes,
            [FieldChange::Changed {
                path: "spec.template.spec.containers[app].env[LOG_LEVEL].value".to_owned(),
                old: json!("info"),
                new: json!("debug"),
            }]
        );
    }

    #[test]
    fn added_and_removed_items_are_reported() {
        let live = json!({"env": [{"name": "A", "value": "1"}]});
        let supplied = json!({"env": [{"name": "B", "value": "2"}], "extra": true});

        let changes = diff("", &live, &supplied);

        assert_eq!(
            changes,
            [
                FieldChange::Removed {
                    path: "env[A]".to_owned(),
                    value: json!({"name": "A", "value": "1"})
                },
                FieldChange::Added {
                    path: "env[B]".to_owned(),
                    value: json!({"name": "B", "value": "2"})
                },
                FieldChange::Added {
                    path: "extra".to_owned(),
                    value: json!(true)
                },
            ]
        );
    }

    #[test]
    fn unnamed_lists_compare_as_a_whole() {
        let changes = diff(
            "",
            &json!({"args": ["a", "b"]}),
            &json!({"args": ["b", "a"]}),
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path(), "args");
    }

    /// Server-side fields never make a ConfigMap look changed: only its contents are compared.
    #[test]
    fn config_map_compares_contents_only() {
        let live = json!({
            "kind": "ConfigMap",
            "metadata": {"name": "cm", "resourceVersion": "42", "uid": "u", "managedFields": []},
            "data": {"PORT": "80", "DEBUG": "true"},
        });
        let supplied = json!({
            "kind": "ConfigMap",
            "metadata": {"name": "cm", "labels": {"team": "x"}},
            "data": {"PORT": 80, "DEBUG": true},
        });

        assert_eq!(
            diff(
                "",
                &comparable_config_map(&live),
                &comparable_config_map(&supplied)
            ),
            []
        );

        let changed = json!({"data": {"PORT": "81", "DEBUG": "true"}});
        assert_eq!(
            diff(
                "",
                &comparable_config_map(&live),
                &comparable_config_map(&changed)
            )
            .iter()
            .map(FieldChange::path)
            .collect::<Vec<_>>(),
            ["data.PORT"]
        );
    }

    /// `stringData` in a manifest and base64 `data` in the cluster are the same Secret when the
    /// bytes match, and the type defaults to `Opaque` on both sides.
    #[test]
    fn secret_string_data_matches_live_base64_data() {
        let live =
            json!({"type": "Opaque", "data": {"password": BASE64_STANDARD.encode("hunter2")}});
        let supplied = json!({"stringData": {"password": "hunter2"}});

        assert_eq!(
            diff(
                "",
                &comparable_secret(&live).unwrap(),
                &comparable_secret(&supplied).unwrap()
            ),
            []
        );

        let changed = json!({"stringData": {"password": "hunter3"}});
        assert_eq!(
            diff(
                "",
                &comparable_secret(&live).unwrap(),
                &comparable_secret(&changed).unwrap()
            )
            .iter()
            .map(FieldChange::path)
            .collect::<Vec<_>>(),
            ["data.password"]
        );
    }

    #[test]
    fn string_data_overrides_data_like_the_api_server() {
        let secret = json!({
            "data": {"a": BASE64_STANDARD.encode("from-data"), "b": BASE64_STANDARD.encode("kept")},
            "stringData": {"a": "from-string-data"},
        });

        let bytes = secret_bytes(&secret).unwrap();
        assert_eq!(bytes["a"], b"from-string-data");
        assert_eq!(bytes["b"], b"kept");
    }

    #[test]
    fn invalid_base64_names_the_key() {
        let error = secret_bytes(&json!({"data": {"token": "not base64!"}})).unwrap_err();
        assert_eq!(error.key, "token");
    }
}
