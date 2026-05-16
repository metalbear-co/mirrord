//! Interactive wizard that generates a skeleton `mirrord-up.yaml`.
//!
//! The wizard prompts for a small, curated subset of the full schema and
//! writes the result as YAML. For fields the wizard knows about but the
//! user did not customize, the output contains a commented-out template
//! line so the user can discover the field without consulting the docs.
//!
//! Keep the prompt set deliberately small. The values that go *into* each
//! prompt (enum variants, defaults) must always come from the authoritative
//! type definitions in `config.rs` (via `strum::VariantArray`, `Default`,
//! etc.), so adding a new variant downstream never requires editing this
//! file.
//!
//! The hand-rolled emitter is necessary because `serde_yaml` cannot emit
//! comments. A round-trip unit test guards against the wizard's output
//! ever diverging from what [`crate::load_up_config`] can parse.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
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
use serde::Serialize;
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
    let rendered = render_yaml(&cfg);

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
fn parse_ports(s: &str) -> Result<HashSet<u16>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse::<u16>()
                .map_err(|_| format!("`{p}` is not a valid u16 port"))
        })
        .collect()
}

fn prompt_ignore_ports() -> Result<HashSet<u16>, InitError> {
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

// ----- YAML emitter -----

/// Renders the YAML config string written by the wizard.
///
/// Note: this is hand-rolled (not `serde_yaml::to_string`) because we want
/// commented-out template lines for fields the user did not customize. The
/// `round_trips_through_loader` test makes sure the output is still a
/// well-formed `mirrord-up.yaml` according to [`UpConfig`].
fn render_yaml(cfg: &UpConfig) -> String {
    let mut out = String::new();
    render_common(&cfg.common, &mut out);
    render_services(&cfg.services, &mut out);
    out
}

fn render_common(c: &CommonConfig, out: &mut String) {
    let entries: Vec<(&str, bool)> = [
        ("operator", c.operator),
        ("accept_invalid_certificates", c.accept_invalid_certificates),
        ("telemetry", c.telemetry),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();

    if entries.is_empty() {
        return;
    }
    out.push_str("common:\n");
    for (k, v) in entries {
        writeln!(out, "  {k}: {v}").unwrap();
    }
    out.push('\n');
}

fn render_services(services: &HashMap<String, ServiceConfig>, out: &mut String) {
    out.push_str("services:\n");
    for (i, (name, svc)) in services.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_service(name, svc, out);
    }
}

fn render_service(name: &str, svc: &ServiceConfig, out: &mut String) {
    writeln!(out, "  {name}:").unwrap();

    // target — emit the whole block commented if nothing is set, to avoid
    // an empty `target:` mapping (TargetConfig.path has no serde default).
    match (&svc.target.path, &svc.target.namespace) {
        (None, None) => {
            writeln!(out, "    # target:").unwrap();
            writeln!(
                out,
                "    #   path: deployment/foo  # omit to run targetless"
            )
            .unwrap();
            writeln!(
                out,
                "    #   namespace: my-ns      # omit for cluster default"
            )
            .unwrap();
        }
        (path, namespace) => {
            writeln!(out, "    target:").unwrap();
            match path {
                Some(t) => writeln!(out, "      path: {}", fmt_scalar(&t.to_string())).unwrap(),
                None => writeln!(
                    out,
                    "      # path: deployment/foo  # omit to run targetless"
                )
                .unwrap(),
            }
            match namespace {
                Some(ns) => writeln!(out, "      namespace: {}", fmt_scalar(ns)).unwrap(),
                None => {
                    writeln!(out, "      # namespace: my-ns  # omit for cluster default").unwrap()
                }
            }
        }
    }

    // default_mode
    if svc.default_mode != ServiceMode::default() {
        writeln!(out, "    default_mode: {}", svc.default_mode).unwrap();
    } else {
        let supported = ServiceMode::VARIANTS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            out,
            "    # default_mode: {}  # supported: {supported}",
            svc.default_mode
        )
        .unwrap();
    }

    // http_filter
    if let Some(hf) = &svc.http_filter.header_filter {
        writeln!(out, "    http_filter:").unwrap();
        writeln!(out, "      header_filter: {}", fmt_scalar(hf)).unwrap();
    } else {
        writeln!(out, "    # http_filter:").unwrap();
        writeln!(
            out,
            "    #   header_filter: \"...\"  # omit to auto-match the session key"
        )
        .unwrap();
    }

    // env.override
    match &svc.env.r#override {
        Some(map) if map.is_empty().not() => {
            writeln!(out, "    env:").unwrap();
            writeln!(out, "      override:").unwrap();
            for (k, v) in map {
                writeln!(out, "        {k}: {}", fmt_scalar(v)).unwrap();
            }
        }
        _ => {
            writeln!(out, "    # env:").unwrap();
            writeln!(out, "    #   override:").unwrap();
            writeln!(out, "    #     KEY: VALUE").unwrap();
        }
    }

    // ignore_ports
    if svc.ignore_ports.is_empty().not() {
        let parts: Vec<_> = svc.ignore_ports.iter().map(|p| p.to_string()).collect();
        writeln!(out, "    ignore_ports: [{}]", parts.join(", ")).unwrap();
    } else {
        writeln!(out, "    # ignore_ports: []").unwrap();
    }

    // run
    writeln!(out, "    run:").unwrap();
    if svc.run.r#type != RunType::default() {
        writeln!(out, "      type: {}", svc.run.r#type).unwrap();
    } else {
        let supported = RunType::VARIANTS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            out,
            "      # type: {}  # supported: {supported}",
            svc.run.r#type
        )
        .unwrap();
    }
    let cmd_parts: Vec<_> = svc.run.command.iter().map(fmt_scalar).collect();
    writeln!(out, "      command: [{}]", cmd_parts.join(", ")).unwrap();
}

/// Serialize a single scalar value as a YAML token, with quoting/escaping
/// handled by `serde_yaml`. Strips the trailing newline so callers can
/// splice it inline.
fn fmt_scalar<T: Serialize>(v: &T) -> String {
    serde_yaml::to_string(v)
        .expect("scalar serialization is infallible")
        .trim_end_matches('\n')
        .to_owned()
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
        let rendered = render_yaml(&cfg);
        let parsed: UpConfig = serde_yaml::from_str(&rendered)
            .unwrap_or_else(|e| panic!("output failed to parse: {e}\n---\n{rendered}"));
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(parsed.common.operator, Some(false));
        assert_eq!(parsed.common.accept_invalid_certificates, Some(true));
        assert_eq!(parsed.common.telemetry, None);
    }

    #[test]
    fn omits_common_block_when_all_defaults() {
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".to_owned(), sample_service())].into_iter().collect(),
        };
        let out = render_yaml(&cfg);
        assert!(
            !out.contains("common:"),
            "common block should be absent:\n{out}"
        );
    }

    #[test]
    fn no_comments_for_set_common_fields() {
        let cfg = UpConfig {
            common: CommonConfig {
                operator: Some(false),
                accept_invalid_certificates: None,
                telemetry: None,
            },
            services: [("svc".to_owned(), sample_service())].into_iter().collect(),
        };
        let out = render_yaml(&cfg);
        // Only the one set field appears under `common:`, no commented entries for the others.
        let common_block: String = out
            .lines()
            .skip_while(|l| !l.starts_with("common:"))
            .take_while(|l| l.starts_with("common:") || l.starts_with("  "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(common_block.contains("operator: false"));
        assert!(
            !common_block.contains("#"),
            "no commented common fields:\n{common_block}"
        );
    }

    #[test]
    fn unset_service_fields_are_commented() {
        let svc = ServiceConfig {
            target: TargetConfig::default(),
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig::default(),
            ignore_ports: HashSet::new(),
            run: RunConfig {
                r#type: RunType::Exec,
                command: vec!["echo".to_owned()],
            },
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".to_owned(), svc)].into_iter().collect(),
        };
        let out = render_yaml(&cfg);
        assert!(out.contains("# target:"));
        assert!(out.contains("# http_filter:"));
        assert!(out.contains("# env:"));
        assert!(out.contains("# ignore_ports:"));
        assert!(out.contains("# type:"));
        // Round-trip the commented output too.
        let _: UpConfig = serde_yaml::from_str(&out).unwrap();
    }
}
