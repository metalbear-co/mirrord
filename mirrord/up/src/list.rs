//! The view of a `mirrord-up.yaml` that `mirrord up --list --format json` prints for IDE
//! extensions, which build their service pickers and debug launches from it.
//!
//! It is built from the config [`crate::load_up_config`] rendered and resolved, without touching
//! the cluster: a service whose target `mirrord up` would look up by name is listed as
//! [`ListedTarget::Inferred`]. The JSON shape is a contract with the extensions, versioned by
//! [`LISTING_VERSION`].

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use itertools::Itertools;
use mirrord_config::config::EnvKey;
use serde::Serialize;

use crate::{
    UpError,
    config::{RunType, ServiceMode, SpecifiedTarget, TargetConfig, UpConfig},
    kube_context::UpKubeContext,
};

/// The version of the listing's shape. Extensions refuse any other, so a change that breaks
/// them bumps it.
pub const LISTING_VERSION: u32 = 1;

/// Every service of an up file, as `mirrord up` would run it with the same key and flags.
#[derive(Debug, Serialize)]
pub struct UpListing {
    version: u32,
    /// The up file, absolute.
    config_path: PathBuf,
    /// The session key the templates were rendered with.
    key: String,
    /// Sorted by name, so the output is stable between runs.
    services: Vec<ListedService>,
}

#[derive(Debug, Serialize)]
struct ListedService {
    name: Arc<str>,
    skip: bool,
    mode: ServiceMode,
    target: ListedTarget,
    /// The kube context `mirrord up` would use for this service.
    context: Option<Arc<str>>,
    run: ListedRun,
}

/// A service's target, told apart the way `mirrord up` tells them apart before resolving.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ListedTarget {
    /// `target.path` is set.
    Path {
        path: String,
        namespace: Option<Arc<str>>,
    },
    /// `target: none`, so the service runs targetless.
    None,
    /// No `target.path`: `mirrord up` looks a workload up by the service's name, and may prompt.
    Inferred,
}

#[derive(Debug, Serialize)]
struct ListedRun {
    r#type: RunType,
    command: Vec<String>,
    /// Absolute, or `None` when the service runs from wherever `mirrord up` was started.
    directory: Option<PathBuf>,
    pre_command: Vec<String>,
}

impl UpListing {
    /// Lists every service of `config`, skipped ones included, the way `mirrord up` would run
    /// them.
    ///
    /// # Arguments
    ///
    /// * `config` - loaded with [`crate::load_up_config`], so its templates are rendered and its
    ///   `run.directory`s are absolute.
    /// * `config_path` - the up file `config` was loaded from.
    /// * `key` - the key `config` was rendered with.
    /// * `kube_context_arg` - `mirrord up --context`, which wins over every service's own.
    pub fn new(
        config: &UpConfig,
        config_path: &Path,
        key: &EnvKey,
        kube_context_arg: Option<Arc<str>>,
    ) -> Result<Self, UpError> {
        let up_context = UpKubeContext {
            command_arg: kube_context_arg,
            common_context: config.common.context.clone(),
        };

        let services = config
            .services
            .iter()
            .sorted_by_key(|(name, _)| *name)
            .map(|(name, service)| ListedService {
                name: name.clone(),
                skip: service.skip,
                mode: service.default_mode,
                target: match &service.target {
                    TargetConfig::Targetless => ListedTarget::None,
                    TargetConfig::Specified(SpecifiedTarget {
                        path: Some(path),
                        namespace,
                    }) => ListedTarget::Path {
                        path: path.to_string(),
                        namespace: namespace.clone(),
                    },
                    TargetConfig::Specified(SpecifiedTarget { path: None, .. }) => {
                        ListedTarget::Inferred
                    }
                },
                context: up_context.get_context(service.context.clone()),
                run: ListedRun {
                    r#type: service.run.r#type.clone(),
                    command: service.run.command.clone(),
                    directory: service.run.directory.clone(),
                    pre_command: service.run.pre_command.clone(),
                },
            })
            .collect();

        Ok(Self {
            version: LISTING_VERSION,
            config_path: dunce::canonicalize(config_path)?,
            key: key.as_str().to_owned(),
            services,
        })
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::load_up_config;

    /// Writes `yaml` as `mirrord-up.yaml` in a fresh directory that also holds an `api` folder.
    fn up_file(yaml: &str) -> (TempDir, PathBuf) {
        let directory = TempDir::new().expect("create a temp dir");
        std::fs::create_dir(directory.path().join("api")).expect("create the api folder");
        let config_path = directory.path().join("mirrord-up.yaml");
        std::fs::write(&config_path, yaml).expect("write the up file");
        (directory, config_path)
    }

    fn listing(config_path: &Path, kube_context_arg: Option<&str>) -> serde_json::Value {
        let key = EnvKey::Provided("gabriela".to_owned());
        let config = load_up_config(config_path, &key).expect("load the up file");
        let listing = UpListing::new(&config, config_path, &key, kube_context_arg.map(Into::into))
            .expect("list the up file");
        serde_json::to_value(listing).expect("the listing is JSON")
    }

    /// The whole document, as the extensions parse it: rendered templates, every target kind,
    /// skipped services included, services sorted by name, and an absolute `run.directory`
    /// resolved against the up file's folder.
    #[test]
    fn the_listing_has_the_shape_extensions_parse() {
        let (directory, config_path) = up_file(
            r#"
services:
  worker:
    target: none
    skip: true
    default_mode: mirror
    run:
      command: ["python", "worker.py"]
  api:
    target:
      path: deployment/api
      namespace: "ns-{{ key }}"
    context: staging
    run:
      type: exec
      directory: api
      command: ["dotnet", "Api.dll", "--session", "{{ key }}"]
      pre_command: ["dotnet", "build"]
  web:
    target:
      namespace: frontend
    run:
      command: ["npm", "start"]
"#,
        );
        let config_directory = dunce::canonicalize(directory.path()).expect("canonical dir");

        assert_eq!(
            listing(&config_path, None),
            json!({
                "version": 1,
                "config_path": config_directory.join("mirrord-up.yaml"),
                "key": "gabriela",
                "services": [
                    {
                        "name": "api",
                        "skip": false,
                        "mode": "split",
                        "target": {
                            "kind": "path",
                            "path": "deployment/api",
                            "namespace": "ns-gabriela"
                        },
                        "context": "staging",
                        "run": {
                            "type": "exec",
                            "command": ["dotnet", "Api.dll", "--session", "gabriela"],
                            "directory": config_directory.join("api"),
                            "pre_command": ["dotnet", "build"]
                        }
                    },
                    {
                        "name": "web",
                        "skip": false,
                        "mode": "split",
                        "target": { "kind": "inferred" },
                        "context": null,
                        "run": {
                            "type": "exec",
                            "command": ["npm", "start"],
                            "directory": null,
                            "pre_command": []
                        }
                    },
                    {
                        "name": "worker",
                        "skip": true,
                        "mode": "mirror",
                        "target": { "kind": "none" },
                        "context": null,
                        "run": {
                            "type": "exec",
                            "command": ["python", "worker.py"],
                            "directory": null,
                            "pre_command": []
                        }
                    }
                ]
            })
        );
    }

    /// `context` is the one `mirrord up` would use: `--context`, else the service's own, else
    /// `common.context`.
    #[rstest::rstest]
    #[case::the_flag_wins(Some("flag"), Some("own"), "flag")]
    #[case::then_the_services_own(None, Some("own"), "own")]
    #[case::then_the_common_one(None, None, "common")]
    fn the_listed_context_is_the_one_up_uses(
        #[case] flag: Option<&str>,
        #[case] own: Option<&str>,
        #[case] expected: &str,
    ) {
        let own = own
            .map(|context| format!("    context: {context}\n"))
            .unwrap_or_default();
        let (_directory, config_path) = up_file(&format!(
            "common:\n  context: common\nservices:\n  api:\n    target: none\n{own}    run:\n      command: [x]\n"
        ));

        assert_eq!(
            listing(&config_path, flag)["services"][0]["context"],
            expected
        );
    }
}
