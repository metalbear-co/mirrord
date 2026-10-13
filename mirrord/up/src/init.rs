//! Interactive wizard that generates a skeleton `mirrord-up.yaml`.
//!
//! The wizard prompts for a small, curated subset of the full schema and
//! writes the result as YAML. It picks each service's target from the
//! cluster of the current kube context.
//!
//! Keep the prompt set deliberately small: ask only for what has no good
//! default. The flow and the wording are meant to be the same as in the
//! `mirrord wizard` GUI, so keep the two in sync.
//!
//! The generated YAML is produced by serializing the assembled [`UpConfig`]
//! with `serde_yaml` and then dropping the entries that are left at their
//! default (see [`remove_defaults`]), so the skeleton contains only the
//! fields the user actually set.

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    ops::Not,
    path::{Path, PathBuf},
    sync::Arc,
};

use inquire::{Confirm, Select, Text, validator::Validation};
use miette::Diagnostic;
use mirrord_config::{
    feature::{env::EnvConfig, network::incoming::http_filter::HttpFilterConfig},
    target::{TargetDisplay, TargetType},
};
use mirrord_kube::{api::kubernetes::create_kube_config, error::KubeApiError};
use thiserror::Error;

use crate::{
    INFERRED_TARGET_TYPES, PickedTarget, UpError,
    config::{
        CommonConfig, RunConfig, RunType, ServiceConfig, ServiceMode, SpecifiedTarget,
        TargetConfig, UpConfig,
    },
    list_namespaces, pick_workload, prompt,
};

/// Errors produced by the `mirrord up init` wizard.
#[derive(Debug, Error, Diagnostic)]
pub enum InitError {
    /// Querying the cluster or prompting the user failed.
    ///
    /// Boxed because [`UpError`] is large, and the error type of the CLI has a
    /// variant for each of the two.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Up(Box<UpError>),

    /// Failed to write the generated config file.
    #[error("failed to write config file: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to deserialize the assembled config to YAML.
    #[error("failed to render config: {0}")]
    YamlDeser(#[from] serde_yaml::Error),

    /// Failed to serialize config YAML.
    #[error("failed to render config: {0}")]
    YamlSer(#[from] serde_saphyr::ser::Error),
}

impl From<UpError> for InitError {
    fn from(error: UpError) -> Self {
        Self::Up(Box::new(error))
    }
}

/// The cluster that the wizard picks targets from.
struct Cluster {
    client: kube::Client,
    namespaces: Vec<String>,
}

impl Cluster {
    /// Connects to the cluster of the current kube context. The namespaces are
    /// listed here, so that an unreachable cluster stops the wizard before it
    /// asks anything.
    async fn connect() -> Result<Self, UpError> {
        let kube_config = create_kube_config(None, None::<&str>, None).await?;
        let client = kube::Client::try_from(kube_config).map_err(KubeApiError::from)?;
        let namespaces = list_namespaces(&client).await?;
        Ok(Self { client, namespaces })
    }
}

/// Run the wizard end-to-end: prompt, render, preview, write.
pub async fn run_wizard(default_output: PathBuf) -> Result<(), InitError> {
    println!("Welcome to `mirrord up init`. This wizard generates a skeleton mirrord-up.yaml.\n");

    let cluster = Cluster::connect().await?;

    let mut services: HashMap<Arc<str>, ServiceConfig> = HashMap::new();
    loop {
        match prompt_service(&cluster, &services).await {
            Ok((name, svc)) => {
                services.insert(name, svc);
            }
            // A service can end with nothing to pick, or with Esc. That must
            // not throw away the services that are done, so the user goes on
            // to the next question. Ctrl-C still ends the wizard.
            Err(InitError::Up(error))
                if services.is_empty().not()
                    && matches!(
                        *error,
                        UpError::Inquire(inquire::InquireError::OperationInterrupted)
                    )
                    .not() =>
            {
                println!("The service was not added: {error}");
            }
            Err(error) => return Err(error),
        }
        if prompt(|| {
            Confirm::new("Add another service?")
                .with_default(false)
                .prompt()
        })
        .await?
        .not()
        {
            break;
        }
    }

    let cfg = UpConfig {
        common: CommonConfig::default(),
        services,
    };
    let rendered = render_yaml(&cfg)?;

    println!("\n--- Preview ---\n{rendered}");

    let Some(path) = prompt(move || prompt_output_path(&default_output)).await? else {
        println!("No file written.");
        return Ok(());
    };

    std::fs::write(&path, &rendered)?;
    println!("Wrote {}.", path.display());
    Ok(())
}

/// Asks whether to save the config and where. Returns `None` when the user
/// does not want to save it.
fn prompt_output_path(default_output: &Path) -> Result<Option<PathBuf>, inquire::InquireError> {
    if Confirm::new("Save configuration to a file?")
        .with_default(true)
        .prompt()?
        .not()
    {
        return Ok(None);
    }

    loop {
        let path: PathBuf = Text::new("Enter filename:")
            .with_default(&default_output.to_string_lossy())
            .prompt()?
            .trim()
            .into();

        if path.exists()
            && Confirm::new(&format!("{} exists. Overwrite?", path.display()))
                .with_default(false)
                .prompt()?
                .not()
        {
            continue;
        }

        return Ok(Some(path));
    }
}

async fn prompt_service(
    cluster: &Cluster,
    existing: &HashMap<Arc<str>, ServiceConfig>,
) -> Result<(Arc<str>, ServiceConfig), InitError> {
    let default_mode = prompt(prompt_mode).await?;
    let (name, picked) = prompt_target(cluster, default_mode, existing).await?;
    // A targetless service has no incoming traffic, so it is not asked about
    // filters and ports.
    let (target, http_filter, ignore_ports) = match picked {
        PickedTarget::Targetless => (
            TargetConfig::Targetless,
            Default::default(),
            Default::default(),
        ),
        PickedTarget::Workload { target, namespace } => {
            let (http_filter, ignore_ports) = prompt_traffic(default_mode).await?;
            // `mirrord up` uses the namespace of the kube context when the
            // field is absent.
            let namespace =
                (namespace != cluster.client.default_namespace()).then(|| namespace.into());
            let target = SpecifiedTarget {
                path: Some(target),
                namespace,
            };
            (target.into(), http_filter, ignore_ports)
        }
    };
    let run = prompt(prompt_run).await?;

    Ok((
        name,
        ServiceConfig {
            target,
            env: EnvConfig::default(),
            default_mode,
            http_filter,
            ignore_ports,
            skip: false,
            run,
            context: None,
            config_patch: None,
        },
    ))
}

/// Picks the target of a service from the cluster, and names the service
/// after it. A targetless service has nothing to be named after, so the user
/// names it.
async fn prompt_target(
    cluster: &Cluster,
    mode: ServiceMode,
    existing: &HashMap<Arc<str>, ServiceConfig>,
) -> Result<(Arc<str>, PickedTarget), InitError> {
    // `replace` copies the workload and scales it down, which only these
    // kinds support (see `TargetType::compatible_with`).
    let kinds: &[TargetType] = match mode {
        ServiceMode::Replace => &[TargetType::Deployment, TargetType::StatefulSet],
        ServiceMode::Split | ServiceMode::Mirror => &INFERRED_TARGET_TYPES,
    };
    // Without a target there is no incoming traffic to split, mirror or
    // replace, so only `split` offers it.
    let targetless = matches!(mode, ServiceMode::Split);

    let picked = pick_workload(&cluster.client, &cluster.namespaces, kinds, targetless).await?;

    let name = match &picked {
        PickedTarget::Workload { target, .. } => {
            let mut name = target.name().to_owned();
            let mut n = 1;
            while existing.contains_key(name.as_str()) {
                n += 1;
                name = format!("{}-{n}", target.name());
            }
            println!("Service name: {name}");
            name
        }
        PickedTarget::Targetless => {
            let existing: Vec<Arc<str>> = existing.keys().cloned().collect();
            prompt(move || {
                Text::new("Service name:")
                    .with_validator(move |s: &str| {
                        let t = s.trim();
                        if t.is_empty() {
                            Ok(Validation::Invalid("name cannot be empty".into()))
                        } else if existing.iter().any(|name| name.as_ref() == t) {
                            Ok(Validation::Invalid(
                                "a service with that name already exists".into(),
                            ))
                        } else {
                            Ok(Validation::Valid)
                        }
                    })
                    .prompt()
            })
            .await?
            .trim()
            .to_owned()
        }
    };

    Ok((name.into(), picked))
}

/// A [`ServiceMode`] as an option of the mode prompt, with its label and its
/// description. The `mirrord wizard` GUI must show the same ones for the
/// three modes, so that both wizards present the same choice.
///
/// The description goes on indented lines under the label. They are wrapped
/// at a fixed width, so that a terminal of 80 columns does not break words.
struct ModeChoice(ServiceMode);

impl ModeChoice {
    const INDENT: &str = "    ";
    const WIDTH: usize = 76;
}

impl fmt::Display for ModeChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (label, description) = match self.0 {
            ServiceMode::Split => (
                "Split (recommended)",
                "Your requests reach your local process. Everyone else's keep hitting the deployed service, so you can work in a shared cluster without disrupting anyone.",
            ),
            ServiceMode::Mirror => (
                "Mirror",
                "A copy of your session's traffic is sent to your local process. The deployed service still handles every request, including yours.",
            ),
            ServiceMode::Replace => (
                "Replace",
                "Every request reaches your local process. The deployed workload is scaled to zero for the session and restored when it ends.",
            ),
        };
        write!(f, "{label}")?;
        // Start as if the line is full, so that the description starts on its
        // own indented line.
        let mut line_width = Self::WIDTH;
        for word in description.split(' ') {
            if line_width + 1 + word.len() > Self::WIDTH {
                write!(f, "\n{}{word}", Self::INDENT)?;
                line_width = Self::INDENT.len() + word.len();
            } else {
                write!(f, " {word}")?;
                line_width += 1 + word.len();
            }
        }
        Ok(())
    }
}

fn prompt_mode() -> Result<ServiceMode, inquire::InquireError> {
    let choices = [
        ServiceMode::Split,
        ServiceMode::Mirror,
        ServiceMode::Replace,
    ]
    .map(ModeChoice)
    .into();

    let choice = Select::new("How do you want to interact with remote traffic?", choices)
        .with_formatter(&|choice| choice.value.0.to_string())
        .prompt()?;
    Ok(choice.0)
}

/// Asks for the HTTP filter and the ports to ignore, only when the user wants
/// to set them. The defaults fit most services, so the question defaults to
/// no.
async fn prompt_traffic(mode: ServiceMode) -> Result<(HttpFilterConfig, BTreeSet<u16>), InitError> {
    // `replace` ignores the HTTP filter.
    let message = match mode {
        ServiceMode::Replace => "Configure ports?",
        ServiceMode::Split | ServiceMode::Mirror => "Configure traffic filtering and ports?",
    };
    if prompt(move || Confirm::new(message).with_default(false).prompt())
        .await?
        .not()
    {
        return Ok(Default::default());
    }

    let http_filter = match mode {
        ServiceMode::Replace => HttpFilterConfig::default(),
        ServiceMode::Split | ServiceMode::Mirror => prompt(prompt_http_filter).await?,
    };
    let ignore_ports = prompt(prompt_ignore_ports).await?;

    Ok((http_filter, ignore_ports))
}

fn prompt_http_filter() -> Result<HttpFilterConfig, inquire::InquireError> {
    let s = Text::new("HTTP header filter (regex; blank for auto session-key filter):")
        .with_help_message("Example: `session-id: my-session-identifier`")
        .prompt()?;

    Ok(match s.trim() {
        "" => HttpFilterConfig::default(),
        hf => HttpFilterConfig {
            header_filter: Some(hf.to_owned()),
            ..Default::default()
        },
    })
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

fn prompt_ignore_ports() -> Result<BTreeSet<u16>, inquire::InquireError> {
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

/// The run type is not asked: `exec` fits most services, and the docs tell
/// container users to set `run.type`.
fn prompt_run() -> Result<RunConfig, inquire::InquireError> {
    let command_str = Text::new("Local command (e.g. `go run ./cmd/api`):")
        .with_validator(|s: &str| {
            if s.split_whitespace().next().is_none() {
                Ok(Validation::Invalid("command cannot be empty".into()))
            } else {
                Ok(Validation::Valid)
            }
        })
        .prompt()?;
    let command = command_str
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect();

    let directory = match Text::new("Working directory (blank for the current directory):")
        .with_help_message(
            "Relative to the directory of mirrord-up.yaml. Set it when the service is in a subdirectory, e.g. in a monorepo.",
        )
        .prompt()?
        .trim()
    {
        "" => None,
        path => Some(path.into()),
    };

    Ok(RunConfig {
        r#type: RunType::Exec,
        directory,
        command,
    })
}

/// Serializes the wizard's [`UpConfig`] to a minimal YAML skeleton.
///
/// `serde_yaml` emits every field, including `null` for the many unset
/// `Option`s in the shared `EnvConfig`/`HttpFilterConfig` types, and fields
/// left at their default, like `skip: false`. [`remove_defaults`] strips those
/// so the output only contains what the user actually set. We can't suppress
/// them at the type level: the `Option`s are in types shared with the rest of
/// `mirrord-config` (config sent to child processes, analytics, schema), and a
/// `skip_serializing_if` on a defaulted field also drops its `default` from
/// the JSON schema.
///
/// The `command` and `ignore_ports` lists are rendered inline (`[a, b]`)
/// rather than as block sequences - see [`inline_lists`].
fn render_yaml(cfg: &UpConfig) -> Result<String, InitError> {
    let mut value = serde_yaml::to_value(cfg)?;

    // What serde fills in for the absent fields of the file, and of a service
    // that sets only its required field.
    let defaults = serde_yaml::to_value(UpConfig {
        common: CommonConfig::default(),
        services: HashMap::new(),
    })?;
    remove_defaults(&mut value, &defaults);
    let service_defaults =
        serde_yaml::to_value(serde_yaml::from_str::<ServiceConfig>("run: {command: []}")?)?;
    if let Some(serde_yaml::Value::Mapping(services)) = value.get_mut("services") {
        for (_, service) in services.iter_mut() {
            remove_defaults(service, &service_defaults);
        }
    }

    let mut flows = Vec::new();
    inline_lists(&mut value, &mut flows);

    let mut yaml = serde_saphyr::to_string(&value)?;
    for (placeholder, flow) in flows {
        yaml = yaml.replace(&placeholder, &flow);
    }
    Ok(yaml)
}

/// Recursively removes the mapping entries of `value` that are equal to the
/// entry at the same place in `defaults`. Deserializing fills the removed
/// entries back in, so the result still round-trips through
/// [`crate::load_up_config`].
fn remove_defaults(value: &mut serde_yaml::Value, defaults: &serde_yaml::Value) {
    let (serde_yaml::Value::Mapping(map), serde_yaml::Value::Mapping(defaults)) = (value, defaults)
    else {
        return;
    };

    map.retain(|key, v| match defaults.get(key) {
        Some(default) if default == v => false,
        Some(default) => {
            remove_defaults(v, default);
            true
        }
        None => true,
    });
}

/// Rewrites the certain fields to inline flow style (`[a, b, c]`)
/// instead of `serde_yaml`'s block style (`- a` per line).
///
/// `serde_yaml` has no native way to force a list inline, so we engage in a bit
/// of tomfoolery: each targeted sequence is swapped for a unique placeholder
/// scalar here, which the caller substitutes for the rendered flow array.
fn inline_lists(value: &mut serde_yaml::Value, flows: &mut Vec<(String, String)>) {
    const INLINE_LIST_KEYS: [&str; 2] = ["command", "ignore_ports"];

    match value {
        serde_yaml::Value::Mapping(map) => {
            for (key, v) in map.iter_mut() {
                let targeted = matches!(key, serde_yaml::Value::String(k) if INLINE_LIST_KEYS.contains(&k.as_str()));
                match v {
                    serde_yaml::Value::Sequence(seq) if targeted && seq.iter().all(is_scalar) => {
                        *v = flow_placeholder(seq, flows);
                    }
                    other => inline_lists(other, flows),
                }
            }
        }
        serde_yaml::Value::Sequence(seq) => {
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
fn flow_placeholder(
    seq: &[serde_yaml::Value],
    flows: &mut Vec<(String, String)>,
) -> serde_yaml::Value {
    let items = seq
        .iter()
        .map(|item| serde_json::to_string(item).expect("scalar to JSON is infallible"));
    let flow = format!("[{}]", items.collect::<Vec<_>>().join(", "));
    let placeholder = format!("__mirrord_up_flow_{}__", flows.len());
    flows.push((placeholder.clone(), flow));
    serde_yaml::Value::String(placeholder)
}

/// Whether a [`serde_yaml::Value`] is a leaf scalar (so a sequence of these can be safely
/// rendered inline by [`flow_placeholder`]).
fn is_scalar(value: &serde_yaml::Value) -> bool {
    matches!(
        value,
        serde_yaml::Value::Null
            | serde_yaml::Value::Bool(_)
            | serde_yaml::Value::Number(_)
            | serde_yaml::Value::String(_)
    )
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::str::FromStr;

    use mirrord_config::target::Target;

    use super::*;

    fn sample_service() -> ServiceConfig {
        ServiceConfig {
            target: TargetConfig::Specified(SpecifiedTarget {
                path: Some(Target::from_str("deployment/api").unwrap()),
                namespace: Some("staging".into()),
            }),
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
            skip: false,
            run: RunConfig {
                r#type: RunType::Exec,
                directory: Some("services/api".into()),
                command: vec!["go".to_owned(), "run".to_owned(), "./cmd/api".to_owned()],
            },
            context: Some("popper-deskpop".into()),
            config_patch: None,
        }
    }

    #[test]
    fn round_trips_through_loader() {
        let cfg = UpConfig {
            common: CommonConfig {
                operator: Some(false),
                accept_invalid_certificates: Some(true),
                telemetry: None,
                context: None,
            },
            services: [("api".into(), sample_service())].into_iter().collect(),
        };
        let rendered = render_yaml(&cfg).unwrap();
        // Target renders in its string form, not as a nested `{deployment: api}` map.
        assert!(
            rendered.contains("path: deployment/api"),
            "target path should be a string:\n{rendered}"
        );
        assert!(rendered.contains("directory: services/api"));
        let parsed: UpConfig = serde_saphyr::from_str(&rendered)
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
            services: [("svc".into(), sample_service())].into_iter().collect(),
        };
        let out = render_yaml(&cfg).unwrap();
        assert!(
            !out.contains("common:"),
            "common block should be absent:\n{out}"
        );
    }

    /// A service that sets only a header filter and a command must not leak the
    /// many defaulted `Option`s of the shared config types as `null`s, nor emit
    /// empty `env`/`ignore_ports`/`target` blocks or fields left at their default.
    #[test]
    fn no_nulls_or_empty_blocks() {
        let svc = ServiceConfig {
            target: TargetConfig::UNSPECIFIED,
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig {
                header_filter: Some("x-session: me".to_owned()),
                ..Default::default()
            },
            ignore_ports: BTreeSet::new(),
            skip: false,
            run: RunConfig {
                r#type: RunType::Exec,
                directory: None,
                command: vec!["echo".to_owned()],
            },
            context: None,
            config_patch: None,
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".into(), svc.clone())].into_iter().collect(),
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
        assert!(!out.contains("context:"), "no empty context:\n{out}");
        assert!(!out.contains("skip:"), "no default skip:\n{out}");
        assert!(
            !out.contains("default_mode:"),
            "no default default_mode:\n{out}"
        );
        assert!(!out.contains("type:"), "no default run type:\n{out}");

        let parsed: UpConfig = serde_saphyr::from_str(&out).unwrap();
        assert_eq!(parsed.services["svc"], svc);
    }

    /// Scalar lists (`command`, `ignore_ports`) render inline (`[a, b]`), not as
    /// block sequences, and tokens with flow-significant chars stay intact.
    #[test]
    fn scalar_lists_render_inline() {
        let svc = ServiceConfig {
            target: TargetConfig::UNSPECIFIED,
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig::default(),
            ignore_ports: [9090, 9091, 15090].into_iter().collect(),
            skip: false,
            run: RunConfig {
                r#type: RunType::Exec,
                directory: None,
                command: vec!["go".to_owned(), "run".to_owned(), "--opt=a,b".to_owned()],
            },
            context: None,
            config_patch: None,
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".into(), svc.clone())].into_iter().collect(),
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
        let parsed: UpConfig = serde_saphyr::from_str(&out).unwrap();
        assert_eq!(parsed.services["svc"], svc);
    }

    /// A namespace-only target prunes `path: null`; the result must still parse
    /// (the `path` field uses a custom deserializer, so it needs `#[serde(default)]`).
    #[test]
    fn namespace_only_target_round_trips() {
        let svc = ServiceConfig {
            target: TargetConfig::Specified(SpecifiedTarget {
                path: None,
                namespace: Some("staging".into()),
            }),
            env: EnvConfig::default(),
            default_mode: ServiceMode::default(),
            http_filter: HttpFilterConfig::default(),
            ignore_ports: BTreeSet::new(),
            skip: false,
            run: RunConfig {
                r#type: RunType::Exec,
                directory: None,
                command: vec!["echo".to_owned()],
            },
            context: None,
            config_patch: None,
        };
        let cfg = UpConfig {
            common: CommonConfig::default(),
            services: [("svc".into(), svc.clone())].into_iter().collect(),
        };
        let out = render_yaml(&cfg).unwrap();
        assert!(!out.contains("path"), "path: null should be pruned:\n{out}");
        let parsed: UpConfig = serde_saphyr::from_str(&out).unwrap();
        assert_eq!(parsed.services["svc"], svc);
    }
}
