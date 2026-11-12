//! What the user picked for each service, and how it becomes the skeleton
//! `mirrord-up.yaml`. Both `mirrord up init` and the `mirrord wizard` GUI (via
//! the `mirrord ui` server) render with [`render_skeleton`], so the same picks
//! give the same file.

use std::{
    collections::{BTreeSet, HashSet},
    ops::Not,
    str::FromStr,
    sync::Arc,
};

use miette::Diagnostic;
use mirrord_config::{
    feature::{env::EnvConfig, network::incoming::http_filter::HttpFilterConfig},
    target::{Target, TargetType},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{RenderError, render_yaml};
use crate::config::{
    CommonConfig, RunConfig, RunType, ServiceConfig, ServiceMode, SpecifiedTarget, TargetConfig,
    UpConfig,
};

/// What the user picked for one service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PickedService {
    /// The service's key in the file.
    pub name: String,

    /// The service's mode.
    pub mode: ServiceMode,

    /// `None` runs the service targetless.
    pub target: Option<PickedWorkload>,

    /// `None` or blank keeps the default session-key filter. Ignored in
    /// `replace`.
    #[serde(default)]
    pub header_filter: Option<String>,

    /// Ports whose incoming traffic is left alone.
    #[serde(default)]
    pub ignore_ports: BTreeSet<u16>,

    /// The local command, split on whitespace.
    pub command: String,

    /// Relative to the file. `None` or blank means the file's directory.
    #[serde(default)]
    pub directory: Option<String>,
}

/// A workload picked from the cluster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PickedWorkload {
    /// Like `deployment/api`.
    pub path: String,

    /// The workload's namespace.
    pub namespace: String,
}

/// Picks that make no valid `mirrord-up.yaml`. The `up init` prompts rule
/// these out; the `mirrord ui` server returns them to the GUI.
#[derive(Debug, Error, Diagnostic)]
pub enum SkeletonError {
    /// No services.
    #[error("add at least one service")]
    NoServices,

    /// A blank service name.
    #[error("a service name cannot be empty")]
    EmptyName,

    /// Two services with one name.
    #[error("there are two services named `{0}`")]
    DuplicateName(String),

    /// A blank command.
    #[error("service `{0}`: the command cannot be empty")]
    EmptyCommand(String),

    /// No target, in a mode that needs one.
    #[error("service `{service}`: `{mode}` needs a target")]
    TargetRequired {
        /// The service.
        service: String,
        /// Its mode.
        mode: ServiceMode,
    },

    /// A target path that does not parse.
    #[error("service `{service}`: invalid target `{path}`")]
    InvalidTarget {
        /// The service.
        service: String,
        /// The target path.
        path: String,
    },

    /// A target kind the mode does not allow.
    #[error("service `{service}`: `{mode}` cannot target a {kind}")]
    KindNotAllowed {
        /// The service.
        service: String,
        /// Its mode.
        mode: ServiceMode,
        /// The kind of its target.
        kind: TargetType,
    },

    /// Serializing the config failed.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Render(#[from] RenderError),
}

/// The workload name, or `<workload>-2`, `-3`, ... when `taken`. Services are a
/// map by name, so a second service on a workload would replace the first.
pub fn service_name_for(workload: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut name = workload.to_owned();
    let mut suffix = 2;
    while taken(&name) {
        name = format!("{workload}-{suffix}");
        suffix += 1;
    }
    name
}

/// Renders the skeleton `mirrord-up.yaml`. A target in `default_namespace`
/// (the kube context's) omits its namespace, which `mirrord up` falls back on.
pub fn render_skeleton(
    services: &[PickedService],
    default_namespace: &str,
) -> Result<String, SkeletonError> {
    if services.is_empty() {
        return Err(SkeletonError::NoServices);
    }

    let mut names = HashSet::new();
    let services = services
        .iter()
        .map(|service| {
            let name = service.name.trim();
            if name.is_empty() {
                return Err(SkeletonError::EmptyName);
            }
            if names.insert(name).not() {
                return Err(SkeletonError::DuplicateName(name.to_owned()));
            }
            Ok((
                Arc::from(name),
                service.to_service_config(default_namespace)?,
            ))
        })
        .collect::<Result<_, _>>()?;

    let cfg = UpConfig {
        common: CommonConfig::default(),
        modes: Default::default(),
        services,
    };
    Ok(render_yaml(&cfg)?)
}

impl PickedService {
    fn to_service_config(&self, default_namespace: &str) -> Result<ServiceConfig, SkeletonError> {
        let service = || self.name.trim().to_owned();

        let command: Vec<String> = self
            .command
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect();
        if command.is_empty() {
            return Err(SkeletonError::EmptyCommand(service()));
        }

        // Without a target there is no incoming traffic to filter.
        let (target, http_filter, ignore_ports) = match &self.target {
            None if self.mode.allows_targetless() => (
                TargetConfig::Targetless,
                HttpFilterConfig::default(),
                BTreeSet::new(),
            ),
            None => {
                return Err(SkeletonError::TargetRequired {
                    service: service(),
                    mode: self.mode,
                });
            }
            Some(PickedWorkload { path, namespace }) => {
                let target =
                    Target::from_str(path.trim()).map_err(|_| SkeletonError::InvalidTarget {
                        service: service(),
                        path: path.clone(),
                    })?;
                let kind = TargetType::from(&target);
                if self.mode.target_kinds().contains(&kind).not() {
                    return Err(SkeletonError::KindNotAllowed {
                        service: service(),
                        mode: self.mode,
                        kind,
                    });
                }

                let header_filter = self
                    .header_filter
                    .as_deref()
                    .map(str::trim)
                    .filter(|filter| self.mode.uses_http_filter() && filter.is_empty().not());
                let http_filter = HttpFilterConfig {
                    header_filter: header_filter.map(ToOwned::to_owned),
                    ..Default::default()
                };

                let namespace = (namespace != default_namespace).then(|| namespace.as_str().into());
                let target = SpecifiedTarget {
                    path: Some(target),
                    namespace,
                };
                (target.into(), http_filter, self.ignore_ports.clone())
            }
        };

        let directory = self
            .directory
            .as_deref()
            .map(str::trim)
            .filter(|directory| directory.is_empty().not())
            .map(Into::into);

        Ok(ServiceConfig {
            target,
            env: EnvConfig::default(),
            default_mode: self.mode.into(),
            http_filter,
            ignore_ports,
            skip: false,
            run: RunConfig {
                r#type: RunType::Exec,
                directory,
                command,
                pre_command: Vec::new(),
            },
            context: None,
            config_patch: None,
            sync: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_on(path: &str, namespace: &str) -> PickedService {
        PickedService {
            name: "api".to_owned(),
            mode: ServiceMode::Split,
            target: Some(PickedWorkload {
                path: path.to_owned(),
                namespace: namespace.to_owned(),
            }),
            header_filter: None,
            ignore_ports: BTreeSet::new(),
            command: "go run ./cmd/api".to_owned(),
            directory: None,
        }
    }

    #[test]
    fn defaults_write_target_and_command_only() {
        let out = render_skeleton(&[split_on("deployment/api", "default")], "default").unwrap();
        assert_eq!(
            out,
            "services:\n  api:\n    target:\n      path: deployment/api\n    run:\n      command: [\"go\", \"run\", \"./cmd/api\"]\n"
        );
    }

    #[test]
    fn writes_what_the_user_set() {
        let service = PickedService {
            mode: ServiceMode::Mirror,
            header_filter: Some("  x-user: me ".to_owned()),
            ignore_ports: [9090, 15090].into(),
            directory: Some(" services/api ".to_owned()),
            ..split_on("deployment/api", "staging")
        };
        let out = render_skeleton(&[service], "default").unwrap();
        assert!(out.contains("namespace: staging"), "{out}");
        assert!(out.contains("default_mode: mirror"), "{out}");
        assert!(out.contains(r#"header_filter: "x-user: me""#), "{out}");
        assert!(out.contains("ignore_ports: [9090, 15090]"), "{out}");
        assert!(out.contains("directory: services/api"), "{out}");
    }

    #[test]
    fn replace_drops_the_header_filter() {
        let service = PickedService {
            mode: ServiceMode::Replace,
            header_filter: Some("x-user: me".to_owned()),
            ..split_on("deployment/api", "default")
        };
        let out = render_skeleton(&[service], "default").unwrap();
        assert!(out.contains("default_mode: replace"), "{out}");
        assert!(!out.contains("http_filter"), "{out}");
    }

    #[test]
    fn targetless_split() {
        let service = PickedService {
            target: None,
            ignore_ports: [80].into(),
            header_filter: Some("x-user: me".to_owned()),
            ..split_on("deployment/api", "default")
        };
        let out = render_skeleton(&[service], "default").unwrap();
        assert!(out.contains("target: none"), "{out}");
        assert!(!out.contains("ignore_ports"), "{out}");
        assert!(!out.contains("http_filter"), "{out}");
    }

    #[test]
    fn services_are_sorted_by_name() {
        let web = PickedService {
            name: "web".to_owned(),
            ..split_on("deployment/web", "default")
        };
        let api = split_on("deployment/api", "default");
        let out = render_skeleton(&[web, api], "default").unwrap();
        let api_at = out.find("  api:").unwrap();
        let web_at = out.find("  web:").unwrap();
        assert!(api_at < web_at, "{out}");
    }

    #[test]
    fn rejects_invalid_picks() {
        let base = split_on("deployment/api", "default");

        assert!(matches!(
            render_skeleton(&[base.clone(), base.clone()], "default"),
            Err(SkeletonError::DuplicateName(name)) if name == "api"
        ));
        let targetless_mirror = PickedService {
            mode: ServiceMode::Mirror,
            target: None,
            ..base
        };
        assert!(matches!(
            render_skeleton(&[targetless_mirror], "default"),
            Err(SkeletonError::TargetRequired { .. })
        ));
        let replace_pod = PickedService {
            mode: ServiceMode::Replace,
            ..split_on("pod/api", "default")
        };
        assert!(matches!(
            render_skeleton(&[replace_pod], "default"),
            Err(SkeletonError::KindNotAllowed { .. })
        ));
    }

    #[test]
    fn names_a_second_service_on_a_workload() {
        let taken = |existing: &'static [&'static str]| move |name: &str| existing.contains(&name);
        assert_eq!(service_name_for("api", taken(&["web"])), "api");
        assert_eq!(service_name_for("api", taken(&["api", "web"])), "api-2");
        assert_eq!(service_name_for("api", taken(&["api", "api-2"])), "api-3");
    }
}
