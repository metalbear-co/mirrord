//! The view of a `mirrord-up.yaml` that `mirrord up list` prints: every service as `mirrord up`
//! would run it, rendered for people ([`UpListing::render`]) or, with `--format json`, as JSON
//! for IDE extensions, which build their service pickers and debug launches from it.
//!
//! It is built from the config [`crate::load_up_config`] rendered and resolved, without touching
//! the cluster: a service whose target `mirrord up` would look up by name is listed as
//! [`ListedTarget::Inferred`]. The JSON shape is a contract with the extensions, versioned by
//! [`LISTING_VERSION`]. The rendered text is for people only, and may change freely.

use std::{
    ops::Not,
    path::{Path, PathBuf},
    sync::Arc,
};

use itertools::Itertools;
use mirrord_config::{
    config::EnvKey,
    target::{Target, TargetFileConfig},
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    UpError,
    config::{RunType, ServiceMode, SpecifiedTarget, TargetConfig, UpConfig},
    kube_context::UpKubeContext,
};

/// The version of the listing's shape. Extensions refuse any other, so a change that breaks
/// them bumps it.
///
/// A new value of `mode` or `target.kind` counts as breaking: extensions refuse a listing with a
/// value they don't know. New fields don't, since extensions ignore them.
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
    /// The kube context `mirrord up` would use for this service, its `config_patch` included.
    context: Option<Arc<str>>,
    run: ListedRun,
}

/// A service's target, its `config_patch` included, told apart the way `mirrord up` tells them
/// apart before resolving.
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
    Inferred {
        /// Where it looks, when the up file names a namespace.
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<Arc<str>>,
    },
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
                target: ListedTarget::of(&service.target, service.config_patch.as_ref()),
                context: patched_context(service.config_patch.as_ref())
                    .unwrap_or_else(|| up_context.get_context(service.context.clone())),
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

    /// The listing for people to read, as `mirrord up list` prints it: the up file and key, then
    /// one block per service, in name order.
    ///
    /// Each block is the service's name, then aligned key/value rows. Rows that would only
    /// restate a default (`type: exec`, no `run.directory`, no `run.pre_command`) are left out.
    pub fn render(&self) -> String {
        let file_name = self.config_path.file_name().map_or_else(
            || self.config_path.display(),
            |name| Path::new(name).display(),
        );
        let config_directory = self.config_path.parent();

        let mut rendered = format!("{file_name} (key: {})\n", self.key);
        for service in &self.services {
            rendered.push('\n');
            rendered.push_str(&service.name);
            if service.skip {
                rendered.push_str("  (skipped)");
            }
            rendered.push('\n');

            let rows = service.rows(config_directory);
            let width = rows
                .iter()
                .map(|(label, _)| label.len())
                .max()
                .unwrap_or_default();
            for (label, value) in rows {
                rendered.push_str(&format!("  {label:<width$}  {value}\n"));
            }
        }
        rendered
    }
}

impl ListedService {
    /// The key/value rows [`UpListing::render`] shows for this service.
    fn rows(&self, config_directory: Option<&Path>) -> Vec<(&'static str, String)> {
        let mut rows = vec![
            ("target", self.target.describe(&self.name)),
            ("mode", self.mode.to_string()),
            ("run", shown_words(&self.run.command)),
        ];
        if self.run.r#type != RunType::Exec {
            rows.push(("type", self.run.r#type.to_string()));
        }
        if let Some(directory) = &self.run.directory {
            rows.push(("in", shown_directory(directory, config_directory)));
        }
        if self.run.pre_command.is_empty().not() {
            rows.push(("before", shown_words(&self.run.pre_command)));
        }
        rows.push((
            "context",
            self.context
                .as_deref()
                .unwrap_or("current kubeconfig context")
                .to_owned(),
        ));
        rows
    }
}

/// The kube context a service's `config_patch` sets: `Some(None)` for one it clears, and `None`
/// when it leaves it alone.
///
/// `mirrord up` merges the patch after it picks the context, so the patch wins over `--context`,
/// the service's own and `common.context`.
fn patched_context(patch: Option<&Value>) -> Option<Option<Arc<str>>> {
    match patch?.get("kube_context")? {
        Value::String(context) => Some(Some(Arc::from(context.as_str()))),
        Value::Null => Some(None),
        // Not a context at all: `mirrord up` refuses the patch when the service runs.
        _ => None,
    }
}

impl ListedTarget {
    /// What `mirrord up` targets for a service with `target` and `patch` (its `config_patch`).
    ///
    /// `mirrord up` merges the patch into the service's config as a JSON merge patch after it sets
    /// the target, so the patch's `target` wins, field by field. The same merge is done here on
    /// the target alone, offline. A merge mirrord can't read is listed unpatched: `mirrord up`
    /// refuses that service when it runs.
    fn of(target: &TargetConfig, patch: Option<&Value>) -> Self {
        let unpatched = match target {
            TargetConfig::Targetless => Self::None,
            TargetConfig::Specified(SpecifiedTarget {
                path: Some(path),
                namespace,
            }) => Self::Path {
                path: path.to_string(),
                namespace: namespace.clone(),
            },
            TargetConfig::Specified(SpecifiedTarget {
                path: None,
                namespace,
            }) => Self::Inferred {
                namespace: namespace.clone(),
            },
        };
        let Some(patch) = patch.and_then(|patch| patch.get("target")) else {
            return unpatched;
        };

        let mut merged = match &unpatched {
            Self::None => json!({ "path": "targetless" }),
            Self::Path { path, namespace } => json!({ "path": path, "namespace": namespace }),
            Self::Inferred { namespace } => json!({ "namespace": namespace }),
        };
        json_patch::merge(&mut merged, patch);
        let (path, namespace) = match serde_json::from_value(merged) {
            Ok(TargetFileConfig::Simple(path)) => (path, None),
            Ok(TargetFileConfig::Advanced { path, namespace }) => (path, namespace),
            Err(_) => return unpatched,
        };
        let namespace = namespace.map(Arc::from);
        match path {
            Some(Target::Targetless) => Self::None,
            Some(path) => Self::Path {
                path: path.to_string(),
                namespace,
            },
            None => Self::Inferred { namespace },
        }
    }

    /// What `mirrord up` targets for the service called `service`, in words.
    fn describe(&self, service: &str) -> String {
        match self {
            Self::Path {
                path,
                namespace: Some(namespace),
            } => format!("{path} in {namespace}"),
            Self::Path {
                path,
                namespace: None,
            } => path.clone(),
            Self::None => "none (targetless)".to_owned(),
            Self::Inferred {
                namespace: Some(namespace),
            } => format!("looked up by the name {service} in {namespace} when it runs"),
            Self::Inferred { namespace: None } => {
                format!("looked up by the name {service} when it runs")
            }
        }
    }
}

/// `words` joined by spaces, each one that is empty or holds whitespace in double quotes, so a
/// reader can tell where one ends.
fn shown_words(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            if word.is_empty() || word.contains(char::is_whitespace) {
                format!("\"{}\"", word.replace('"', "\\\""))
            } else {
                word.clone()
            }
        })
        .join(" ")
}

/// `directory` relative to the up file's folder when it is inside it, with `/` separators and a
/// trailing `/` (`./` for the folder itself), else as it is.
fn shown_directory(directory: &Path, config_directory: Option<&Path>) -> String {
    let Some(relative) = config_directory.and_then(|base| directory.strip_prefix(base).ok()) else {
        return directory.display().to_string();
    };
    let parts = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "./".to_owned()
    } else {
        format!("{}/", parts.join("/"))
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
        let (directory, config_path) = up_file(include_str!("testdata/list.yaml"));
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
                        "target": { "kind": "inferred", "namespace": "frontend" },
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
        let (_directory, config_path) = up_file(include_str!("testdata/list-context.yaml"));
        let key = EnvKey::Provided("gabriela".to_owned());
        let mut config = load_up_config(&config_path, &key).expect("load the up file");
        config
            .services
            .get_mut("api")
            .expect("the fixture has an `api` service")
            .context = own.map(Arc::from);
        let listing = UpListing::new(&config, &config_path, &key, flag.map(Arc::from))
            .expect("list the up file");

        assert_eq!(
            serde_json::to_value(listing).expect("the listing is JSON")["services"][0]["context"],
            expected
        );
    }

    /// The whole text `mirrord up list` prints: every target kind, a skipped service, a container
    /// with an argument that needs quotes, `run.pre_command`, a directory shown relative to the up
    /// file, and services with no context of their own.
    #[test]
    fn the_rendered_listing_reads_as_people_expect() {
        let (_directory, config_path) = up_file(include_str!("testdata/list-pretty.yaml"));
        let key = EnvKey::Provided("gabriela".to_owned());
        let config = load_up_config(&config_path, &key).expect("load the up file");
        let listing = UpListing::new(&config, &config_path, &key, None).expect("list the up file");

        assert_eq!(
            listing.render(),
            concat!(
                "mirrord-up.yaml (key: gabriela)\n",
                "\n",
                "api\n",
                "  target   deployment/api in ns-gabriela\n",
                "  mode     split\n",
                "  run      dotnet Api.dll --session gabriela\n",
                "  in       api/\n",
                "  before   dotnet build\n",
                "  context  staging\n",
                "\n",
                "db\n",
                "  target   statefulset/postgres\n",
                "  mode     split\n",
                "  run      docker run -e \"GREETING=hello world\" postgres\n",
                "  type     container\n",
                "  context  current kubeconfig context\n",
                "\n",
                "web\n",
                "  target   looked up by the name web in frontend when it runs\n",
                "  mode     split\n",
                "  run      npm start\n",
                "  context  current kubeconfig context\n",
                "\n",
                "worker  (skipped)\n",
                "  target   none (targetless)\n",
                "  mode     mirror\n",
                "  run      python worker.py\n",
                "  context  current kubeconfig context\n",
            )
        );
    }

    /// A `config_patch` is merged after `mirrord up` picks the target and context, so what it sets
    /// is what's listed: a context set or cleared, and a target changed field by field.
    #[test]
    fn a_config_patch_wins_over_the_target_and_context() {
        let (_directory, config_path) = up_file(include_str!("testdata/list-patched.yaml"));
        let services = &listing(&config_path, Some("flag"))["services"];

        let listed = |index: usize| {
            (
                services[index]["name"].clone(),
                services[index]["target"].clone(),
                services[index]["context"].clone(),
            )
        };
        assert_eq!(
            listed(0),
            (
                json!("api"),
                json!({ "kind": "path", "path": "deployment/api", "namespace": "ns-patched" }),
                json!("production"),
            ),
            "the patch's namespace and context replace the up file's, and beat --context"
        );
        assert_eq!(
            listed(1),
            (json!("cleared"), json!({ "kind": "none" }), Value::Null),
            "a patch that clears the context leaves the current kubeconfig context"
        );
        assert_eq!(
            listed(2),
            (
                json!("worker"),
                json!({ "kind": "path", "path": "deployment/worker", "namespace": null }),
                json!("flag"),
            ),
            "a patch's target replaces `target: none`; with no patched context, --context wins"
        );
    }

    /// A `run.directory` inside the up file's folder is shown relative to it, the way the up file
    /// spells it; one outside is shown as it is.
    #[rstest::rstest]
    #[case::a_subfolder(&["api"], "api/")]
    #[case::a_nested_subfolder(&["api", "src"], "api/src/")]
    #[case::the_folder_itself(&[], "./")]
    fn a_directory_inside_the_up_folder_is_relative(
        #[case] parts: &[&str],
        #[case] expected: &str,
    ) {
        let base = std::env::temp_dir().join("up");
        let directory = parts
            .iter()
            .fold(base.clone(), |path, part| path.join(part));
        assert_eq!(shown_directory(&directory, Some(&base)), expected);
    }

    #[test]
    fn a_directory_outside_the_up_folder_is_absolute() {
        let base = std::env::temp_dir().join("up");
        let elsewhere = std::env::temp_dir().join("elsewhere");
        assert_eq!(
            shown_directory(&elsewhere, Some(&base)),
            elsewhere.display().to_string()
        );
    }
}
