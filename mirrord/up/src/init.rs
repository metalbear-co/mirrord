//! Interactive wizard that generates a skeleton `mirrord-up.yaml`.
//!
//! The wizard prompts for a small, curated subset of the full schema and
//! writes the result as YAML.
//!
//! Keep the prompt set deliberately small. The values that go *into* each
//! prompt (enum variants, defaults) must always come from the authoritative
//! type definitions in `config.rs` (via `strum::VariantArray`, `Default`,
//! etc.), so adding a new variant downstream never requires editing this
//! file.
//!
//! The generated YAML is produced by serializing the assembled [`UpConfig`]
//! with `serde_yaml` and then dropping null/empty entries (see [`prune`]), so
//! the skeleton contains only the fields the user actually set.

use std::{
    collections::{BTreeSet, HashMap},
    ops::Not,
    path::PathBuf,
    str::FromStr,
};

use inquire::{Confirm, Select, Text, validator::Validation};
use miette::Diagnostic;
use mirrord_config::{
    feature::{env::EnvConfig, network::incoming::http_filter::HttpFilterConfig},
    target::Target,
};
use serde_yaml::Value;
use strum::VariantArray;
use thiserror::Error;

use crate::config::{
    CommonConfig, RunConfig, RunType, ServiceConfig, ServiceMode, TargetConfig, UpConfig,
};

/// Errors produced by the `mirrord up init` wizard.
#[derive(Debug, Error, Diagnostic)]
pub enum InitError {
    /// Interactive prompt failed or was cancelled.
    #[error("wizard prompt failed: {0}")]
    Inquire(#[from] inquire::InquireError),

    /// Failed to write the generated config file.
    #[error("failed to write config file: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to serialize the assembled config to YAML.
    #[error("failed to render config: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Run the wizard end-to-end: prompt, render, preview, write.
pub fn run_wizard(default_output: PathBuf) -> Result<(), InitError> {
    println!("Welcome to `mirrord up init`. This wizard generates a skeleton mirrord-up.yaml.\n");

    println!("--- Step 1: Common settings (apply to all services) ---");
    let common = prompt_common()?;

    println!("\n--- Step 2: Services ---");
    let mut services: HashMap<String, ServiceConfig> = HashMap::new();
    loop {
        let (name, svc) = prompt_service(&services)?;
        services.insert(name, svc);
        if Confirm::new("Add another service?")
            .with_default(false)
            .prompt()?
            .not()
        {
            break;
        }
    }

    let cfg = UpConfig { common, services };
    let rendered = render_yaml(&cfg)?;

    println!("\n--- Step 3: Preview ---\n{rendered}");

    let path: PathBuf = Text::new("Save to:")
        .with_default(&default_output.to_string_lossy())
        .prompt()?
        .trim()
        .into();

    if path.exists()
        && !Confirm::new(&format!("{} exists. Overwrite?", path.display()))
            .with_default(false)
            .prompt()?
    {
        println!("Aborted; no file written.");
        return Ok(());
    }

    std::fs::write(&path, &rendered)?;
    println!("Wrote {}.", path.display());
    Ok(())
}

fn prompt_common() -> Result<CommonConfig, InitError> {
    let use_operator = Confirm::new("Use the mirrord operator?")
        .with_default(true)
        .with_help_message("Required for splitting traffic between services.")
        .prompt()?;

    let accept_invalid_certs = Confirm::new("Accept invalid TLS certificates for the operator?")
        .with_default(false)
        .with_help_message("Skip TLS verification. Useful for self-signed certs.")
        .prompt()?;

    let telemetry = Confirm::new("Enable anonymous telemetry?")
        .with_default(true)
        .with_help_message("Sends anonymous usage data to help improve mirrord.")
        .prompt()?;

    Ok(CommonConfig {
        operator: (!use_operator).then_some(false),
        accept_invalid_certificates: accept_invalid_certs.then_some(true),
        telemetry: (!telemetry).then_some(false),
    })
}

fn prompt_service(
    existing: &HashMap<String, ServiceConfig>,
) -> Result<(String, ServiceConfig), InitError> {
    let name = Text::new("Service name:")
        .with_validator(|s: &str| {
            let t = s.trim();
            if t.is_empty() {
                Ok(Validation::Invalid("name cannot be empty".into()))
            } else if existing.contains_key(t) {
                Ok(Validation::Invalid(
                    "a service with that name already exists".into(),
                ))
            } else {
                Ok(Validation::Valid)
            }
        })
        .prompt()?
        .trim()
        .to_owned();

    let target = prompt_target()?;
    let default_mode = prompt_mode()?;
    let http_filter = prompt_http_filter(&default_mode)?;
    let ignore_ports = prompt_ignore_ports()?;
    let env = EnvConfig {
        r#override: prompt_env_overrides()?,
        ..Default::default()
    };
    let run = prompt_run()?;

    Ok((
        name,
        ServiceConfig {
            target,
            env,
            default_mode,
            http_filter,
            ignore_ports,
            run,
        },
    ))
}

fn prompt_target() -> Result<TargetConfig, InitError> {
    let path_str = Text::new("Target (e.g. `deployment/foo`, `pod/bar`; blank for targetless):")
        .with_validator(|s: &str| {
            let t = s.trim();
            if t.is_empty() {
                return Ok(Validation::Valid);
            }
            match Target::from_str(t) {
                Ok(_) => Ok(Validation::Valid),
                Err(e) => Ok(Validation::Invalid(e.to_string().into())),
            }
        })
        .prompt()?;

    let path = match path_str.trim() {
        "" => None,
        t => Some(Target::from_str(t).expect("validated above")),
    };

    let namespace_str = Text::new("Namespace (blank for cluster default):").prompt()?;
    let namespace = match namespace_str.trim() {
        "" => None,
        n => Some(n.to_owned()),
    };

    Ok(TargetConfig { path, namespace })
}

fn prompt_mode() -> Result<ServiceMode, InitError> {
    if ServiceMode::VARIANTS.len() <= 1 {
        return Ok(ServiceMode::default());
    }
    Ok(Select::new("Mode:", ServiceMode::VARIANTS.to_vec()).prompt()?)
}

fn prompt_http_filter(mode: &ServiceMode) -> Result<HttpFilterConfig, InitError> {
    match mode {
        ServiceMode::Split => {
            let s = Text::new("HTTP header filter (regex; blank for auto session-key filter):")
                .with_help_message("Example: `session-id: my-session-identifier`")
                .prompt()?;

            let filter = match s.trim() {
                "" => HttpFilterConfig::default(),
                hf => HttpFilterConfig {
                    header_filter: Some(hf.to_owned()),
                    ..Default::default()
                },
            };

            Ok(filter)
        }
    }
}

/// Parses a comma-separated list of u16 ports. Whitespace and empty entries
/// are ignored. On failure, the error is the user-facing message naming the
/// offending token.
fn parse_ports(s: &str) -> Result<BTreeSet<u16>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| p.is_empty().not())
        .map(|p| {
            p.parse::<u16>()
                .map_err(|_| format!("`{p}` is not a valid u16 port"))
        })
        .collect()
}

fn prompt_ignore_ports() -> Result<BTreeSet<u16>, InitError> {
    let presets: [(&str, &[u16]); _] = [
        ("None", &[]),
        ("Istio sidecar (9090, 9091, 15090)", &[9090, 9091, 15090]),
        ("Linkerd sidecar (4140, 4143, 4191)", &[4140, 4143, 4191]),
        ("Custom...", &[]),
    ];

    let choice = Select::new(
        "Ports to ignore (sidecar ports, etc.):",
        presets.iter().map(|(label, _)| *label).collect(),
    )
    .prompt()?;

    if choice == "Custom..." {
        let s = Text::new("Comma-separated ports:")
            .with_validator(|s: &str| match parse_ports(s) {
                Ok(_) => Ok(Validation::Valid),
                Err(e) => Ok(Validation::Invalid(e.into())),
            })
            .prompt()?;
        return Ok(parse_ports(&s).expect("validated above"));
    }

    let ports = presets
        .iter()
        .find(|(label, _)| *label == choice)
        .map(|(_, ports)| *ports)
        .unwrap_or(&[]);
    Ok(ports.iter().copied().collect())
}

fn prompt_env_overrides() -> Result<Option<HashMap<String, String>>, InitError> {
    let mut map: HashMap<String, String> = HashMap::new();
    while Confirm::new("Add an env-var override for the local process?")
        .with_default(false)
        .prompt()?
    {
        let key = Text::new("  Variable name:")
            .with_validator(|s: &str| {
                if s.trim().is_empty() {
                    Ok(Validation::Invalid("name cannot be empty".into()))
                } else {
                    Ok(Validation::Valid)
                }
            })
            .prompt()?
            .trim()
            .to_owned();
        let value = Text::new("  Value:").prompt()?;
        map.insert(key, value);
    }

    Ok((map.is_empty().not()).then_some(map))
}

fn prompt_run() -> Result<RunConfig, InitError> {
    let r#type = Select::new(
        "Run with `mirrord exec` or `mirrord container`?",
        RunType::VARIANTS.to_vec(),
    )
    .prompt()?;

    let command_str = Text::new("Local command (e.g. `go run ./cmd/api`):")
        .with_validator(|s: &str| {
            if s.split_whitespace().next().is_none() {
                Ok(Validation::Invalid("command cannot be empty".into()))
            } else {
                Ok(Validation::Valid)
            }
        })
        .prompt()?;
    let command = command_str.split_whitespace().map(str::to_owned).collect();

    Ok(RunConfig { r#type, command })
}

/// Serializes the wizard's [`UpConfig`] to a minimal YAML skeleton.
///
/// `serde_yaml` emits every field, including `null` for the many unset
/// `Option`s in the shared `EnvConfig`/`HttpFilterConfig` types. [`prune`]
/// strips those so the output only contains what the user actually set. We
/// can't suppress them at the type level: those types are shared with the rest
/// of `mirrord-config` (config sent to child processes, analytics, schema), so
/// a `skip_serializing_if` there would have far wider reach than this wizard.
///
/// The `command` and `ignore_ports` lists are rendered inline (`[a, b]`)
/// rather than as block sequences - see [`inline_lists`].
fn render_yaml(cfg: &UpConfig) -> Result<String, InitError> {
    let mut value = serde_yaml::to_value(cfg)?;
    prune(&mut value);

    let mut flows = Vec::new();
    inline_lists(&mut value, &mut flows);

    let mut yaml = serde_yaml::to_string(&value)?;
    for (placeholder, flow) in flows {
        yaml = yaml.replace(&placeholder, &flow);
    }
    Ok(yaml)
}

/// Recursively removes `null` mapping entries and entries that become empty
/// maps/sequences, so the generated skeleton omits defaulted fields. Every
/// field dropped here is `Option`-or-defaulted on [`UpConfig`], so the result
/// still round-trips through [`crate::load_up_config`].
fn prune(value: &mut serde_yaml::Value) {
    let serde_yaml::Value::Mapping(map) = value else {
        return;
    };

    for (_, v) in map.iter_mut() {
        prune(v);
    }

    map.retain(|_, v| match v {
        serde_yaml::Value::Null => false,
        serde_yaml::Value::Mapping(m) => m.is_empty().not(),
        serde_yaml::Value::Sequence(s) => s.is_empty().not(),
        _ => true,
    });
}

/// Rewrites the [`INLINE_LIST_KEYS`] lists to inline flow style (`[a, b, c]`)
/// instead of `serde_yaml`'s block style (`- a` per line).
///
/// `serde_yaml` has no native way to force a list inline, so we engage in a bit
/// of tomfoolery: each targeted sequence is swapped for a unique placeholder
/// scalar here, which the caller substitutes for the rendered flow array.
fn inline_lists(value: &mut Value, flows: &mut Vec<(String, String)>) {
    const INLINE_LIST_KEYS: [&str; 2] = ["command", "ignore_ports"];

    match value {
        Value::Mapping(map) => {
            for (key, v) in map.iter_mut() {
                let targeted =
                    matches!(key, Value::String(k) if INLINE_LIST_KEYS.contains(&k.as_str()));
                match v {
                    Value::Sequence(seq) if targeted && seq.iter().all(is_scalar) => {
                        *v = flow_placeholder(seq, flows);
                    }
                    other => inline_lists(other, flows),
                }
            }
        }
        Value::Sequence(seq) => {
            for v in seq.iter_mut() {
                inline_lists(v, flows);
            }
        }
        _ => {}
    }
}

/// Renders a scalar sequence as a flow array and records it against a unique
/// placeholder scalar, which [`render_yaml`] substitutes back in after
/// serialization. The array is rendered as JSON — which is valid YAML flow and
/// quotes each item correctly, so tokens containing `,`/`[`/spaces survive
/// intact (naively joining `serde_yaml`'s block lines would not).
fn flow_placeholder(seq: &[Value], flows: &mut Vec<(String, String)>) -> Value {
    let items = seq
        .iter()
        .map(|item| serde_json::to_string(item).expect("scalar to JSON is infallible"));
    let flow = format!("[{}]", items.collect::<Vec<_>>().join(", "));
    let placeholder = format!("__mirrord_up_flow_{}__", flows.len());
    flows.push((placeholder.clone(), flow));
    Value::String(placeholder)
}

/// Whether a [`Value`] is a leaf scalar (so a sequence of these can be safely
/// rendered inline by [`flow_placeholder`]).
fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sample_service() -> ServiceConfig {
        ServiceConfig {
            target: TargetConfig {
                path: Some(Target::from_str("deployment/api").unwrap()),
                namespace: Some("staging".to_owned()),
            },
            env: EnvConfig {
                r#override: Some([("FOO".to_owned(), "bar".to_owned())].into_iter().collect()),
                ..Default::default()
            },
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig {
                header_filter: Some("x-session: me".to_owned()),
                ..Default::default()
            },
            ignore_ports: [9090, 15090].into_iter().collect(),
            run: RunConfig {
                r#type: RunType::Exec,
                command: vec!["go".to_owned(), "run".to_owned(), "./cmd/api".to_owned()],
            },
        }
    }

    #[test]
    fn round_trips_through_loader() {
        let cfg = UpConfig {
            common: CommonConfig {
                operator: Some(false),
                accept_invalid_certificates: Some(true),
                telemetry: None,
            },
            services: [("api".to_owned(), sample_service())].into_iter().collect(),
        };
        let rendered = render_yaml(&cfg).unwrap();
        // Target renders in its string form, not as a nested `{deployment: api}` map.
        assert!(
            rendered.contains("path: deployment/api"),
            "target path should be a string:\n{rendered}"
        );
        let parsed: UpConfig = serde_yaml::from_str(&rendered)
            .unwrap_or_else(|e| panic!("output failed to parse: {e}\n---\n{rendered}"));
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(parsed.common.operator, Some(false));
        assert_eq!(parsed.common.accept_invalid_certificates, Some(true));
        assert_eq!(parsed.common.telemetry, None);
        assert_eq!(parsed.services["api"], sample_service());
    }

    #[test]
    fn omits_common_when_all_default() {
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".to_owned(), sample_service())].into_iter().collect(),
        };
        let out = render_yaml(&cfg).unwrap();
        assert!(
            !out.contains("common:"),
            "common block should be absent:\n{out}"
        );
    }

    /// A service that sets only a header filter and a command must not leak the
    /// many defaulted `Option`s of the shared config types as `null`s, nor emit
    /// empty `env`/`ignore_ports`/`target` blocks.
    #[test]
    fn no_nulls_or_empty_blocks() {
        let svc = ServiceConfig {
            target: TargetConfig::default(),
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig {
                header_filter: Some("x-session: me".to_owned()),
                ..Default::default()
            },
            ignore_ports: BTreeSet::new(),
            run: RunConfig {
                r#type: RunType::Exec,
                command: vec!["echo".to_owned()],
            },
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".to_owned(), svc.clone())].into_iter().collect(),
        };
        let out = render_yaml(&cfg).unwrap();

        assert!(!out.contains("null"), "no null entries:\n{out}");
        assert!(
            !out.contains("path_filter"),
            "no unset filter fields:\n{out}"
        );
        assert!(!out.contains("env:"), "no empty env block:\n{out}");
        assert!(
            !out.contains("ignore_ports"),
            "no empty ignore_ports:\n{out}"
        );
        assert!(!out.contains("target:"), "no empty target block:\n{out}");
        assert!(out.contains("header_filter"), "filter retained:\n{out}");

        let parsed: UpConfig = serde_yaml::from_str(&out).unwrap();
        assert_eq!(parsed.services["svc"], svc);
    }

    /// Scalar lists (`command`, `ignore_ports`) render inline (`[a, b]`), not as
    /// block sequences, and tokens with flow-significant chars stay intact.
    #[test]
    fn scalar_lists_render_inline() {
        let svc = ServiceConfig {
            target: TargetConfig::default(),
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig::default(),
            ignore_ports: [9090, 9091, 15090].into_iter().collect(),
            run: RunConfig {
                r#type: RunType::Exec,
                command: vec!["go".to_owned(), "run".to_owned(), "--opt=a,b".to_owned()],
            },
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".to_owned(), svc.clone())].into_iter().collect(),
        };
        let out = render_yaml(&cfg).unwrap();

        // No block-sequence lines for these fields.
        assert!(
            !out.lines().any(|l| l.trim_start().starts_with("- ")),
            "no block sequence items:\n{out}"
        );
        assert!(
            out.contains(r#"command: ["go", "run", "--opt=a,b"]"#),
            "command inline with intact comma token:\n{out}"
        );
        assert!(
            out.contains("ignore_ports: [9090, 9091, 15090]"),
            "ignore_ports inline and sorted:\n{out}"
        );

        // The comma token must survive the round-trip as a single argument.
        let parsed: UpConfig = serde_yaml::from_str(&out).unwrap();
        assert_eq!(parsed.services["svc"], svc);
    }

    /// A namespace-only target prunes `path: null`; the result must still parse
    /// (the `path` field uses a custom deserializer, so it needs `#[serde(default)]`).
    #[test]
    fn namespace_only_target_round_trips() {
        let svc = ServiceConfig {
            target: TargetConfig {
                path: None,
                namespace: Some("staging".to_owned()),
            },
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig::default(),
            ignore_ports: BTreeSet::new(),
            run: RunConfig {
                r#type: RunType::Exec,
                command: vec!["echo".to_owned()],
            },
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".to_owned(), svc.clone())].into_iter().collect(),
        };
        let out = render_yaml(&cfg).unwrap();
        assert!(!out.contains("path"), "path: null should be pruned:\n{out}");
        let parsed: UpConfig = serde_yaml::from_str(&out).unwrap();
        assert_eq!(parsed.services["svc"], svc);
    }
}
