//! Tiny per-target launch history.
//!
//! What the user last ran for a target is the best possible suggestion
//! for it - their own prior choice, not a guess. Each run records every
//! service's directory and command under its target; picking the same
//! target again prefills the dialog and ranks the remembered commands
//! first among the suggestions.
//!
//! Persisted best-effort in the XDG state directory (history is state,
//! not configuration - the same place shell history conventionally
//! lives); any I/O or parse failure just means an empty history - it
//! must never break a run.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::screens::targets::model::TargetSpec;

/// How many commands are remembered per target - enough to cycle recent
/// variants without silting up the suggestions row.
const MAX_COMMANDS: usize = 3;

/// One target's remembered launch settings.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetHistory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    #[serde(default)]
    targets: BTreeMap<String, TargetHistory>,
    /// Whole runs from the Run view, newest first, for running one again with a single key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    runs: Vec<RunRecipe>,
}

/// How many whole runs are remembered: the handful someone switches between.
const MAX_RUNS: usize = 10;

/// One run of the Run view, everything needed to set it up again: the folders, the target and
/// command of each, what the config step started from, and the options edited there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecipe {
    /// The kube context it ran against; runs of other clusters are not offered.
    pub cluster: String,
    pub services: Vec<RecipeService>,
    /// What the config step started from: a made config's id (`made:mine`) or `file:<path>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_from: Option<String>,
    /// The options set on the config step, when it was a new config never saved to a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
    /// When it ran, in seconds since 1970.
    pub ran_at: i64,
}

/// One folder of a remembered run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeService {
    pub dir: String,
    /// The target, as `path (namespace)`; `None` ran with no target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The key one target's history lives under:
/// `cluster/namespace/kind/name`, with `-` for an unset namespace. The
/// same target name on two clusters must not share history.
pub fn target_key(target: &TargetSpec) -> String {
    keyed(&cluster(), target)
}

/// The pure key builder; [`target_key`] fills in the active cluster.
fn keyed(cluster: &str, target: &TargetSpec) -> String {
    match target {
        TargetSpec::Targetless => format!("{cluster}/targetless"),
        TargetSpec::Path { path, namespace } => {
            format!("{cluster}/{}/{path}", namespace.as_deref().unwrap_or("-"))
        }
    }
}

/// The active cluster: the kube context the user picked in the TUI, or
/// the kubeconfig's current context otherwise.
pub fn cluster() -> String {
    if let Ok(picked) = picked_cluster().read()
        && let Some(picked) = picked.as_ref()
    {
        return picked.clone();
    }
    static CURRENT: OnceLock<String> = OnceLock::new();
    CURRENT
        .get_or_init(|| {
            kube::config::Kubeconfig::read()
                .ok()
                .and_then(|config| config.current_context)
                .unwrap_or_else(|| "default".to_owned())
        })
        .clone()
}

fn picked_cluster() -> &'static RwLock<Option<String>> {
    static PICKED: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    PICKED.get_or_init(RwLock::default)
}

/// Records the kube context the user switched to, so history keys follow.
pub fn set_cluster(context: Option<String>) {
    if let Ok(mut picked) = picked_cluster().write() {
        *picked = context;
    }
}

/// `$XDG_STATE_HOME/mirrord-tui/history.yaml`, defaulting to
/// `~/.local/state/mirrord-tui/history.yaml`.
fn file() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("mirrord-tui/history.yaml"))
}

fn store() -> &'static RwLock<History> {
    static STORE: OnceLock<RwLock<History>> = OnceLock::new();
    STORE.get_or_init(|| {
        let loaded = file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_saphyr::from_str(&text).ok())
            .unwrap_or_default();
        RwLock::new(loaded)
    })
}

/// The remembered settings for a target, if it ever ran.
pub fn recall(key: &str) -> Option<TargetHistory> {
    store().read().ok()?.targets.get(key).cloned()
}

/// Every folder something ran from, on any cluster, without duplicates. The folder picker lists
/// these first: a folder someone ran before is the one they most likely want again.
pub fn dirs() -> Vec<String> {
    let Ok(history) = store().read() else {
        return Vec::new();
    };
    let mut dirs: Vec<String> = history
        .targets
        .values()
        .filter_map(|entry| entry.dir.clone())
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Records one launched service and persists the history.
pub fn record(key: String, dir: Option<String>, command: String) {
    let Ok(mut history) = store().write() else {
        return;
    };
    remember(history.targets.entry(key).or_default(), dir, command);
    persist(&history);
}

/// Records a whole run of the Run view (stamped with the current cluster) and persists it.
pub fn record_run(mut recipe: RunRecipe) {
    recipe.cluster = cluster();
    let Ok(mut history) = store().write() else {
        return;
    };
    remember_run(&mut history.runs, recipe);
    persist(&history);
}

/// The remembered runs on the current cluster, newest first.
pub fn runs() -> Vec<RunRecipe> {
    let cluster = cluster();
    store()
        .read()
        .map(|history| {
            history
                .runs
                .iter()
                .filter(|run| run.cluster == cluster)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Best effort, like everything here: a history that cannot be written only means a shorter
/// memory next time.
fn persist(history: &History) {
    let Some(path) = file() else { return };
    if let Some(parent) = path.parent() {
        _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_saphyr::to_string(history) {
        _ = std::fs::write(path, text);
    }
}

/// The pure update for runs: newest first, the same setup run again moves to the front instead
/// of repeating, and the list stays capped.
fn remember_run(runs: &mut Vec<RunRecipe>, recipe: RunRecipe) {
    runs.retain(|known| {
        (&known.cluster, &known.services, &known.start_from)
            != (&recipe.cluster, &recipe.services, &recipe.start_from)
    });
    runs.insert(0, recipe);
    runs.truncate(MAX_RUNS);
}

/// The pure update: newest command first, duplicates collapse, the list
/// stays capped, and the directory tracks the latest run.
fn remember(entry: &mut TargetHistory, dir: Option<String>, command: String) {
    entry.dir = dir;
    entry.commands.retain(|known| *known != command);
    entry.commands.insert(0, command);
    entry.commands.truncate(MAX_COMMANDS);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &str, at: i64) -> RunRecipe {
        RunRecipe {
            cluster: "kind".to_owned(),
            services: vec![RecipeService {
                dir: dir.to_owned(),
                target: Some("deployment/api (shop)".to_owned()),
                command: Some("npm run dev".to_owned()),
            }],
            start_from: Some("made:mine".to_owned()),
            options: None,
            ran_at: at,
        }
    }

    #[test]
    fn the_same_run_again_moves_to_the_front_and_the_list_stays_capped() {
        let mut runs = Vec::new();
        remember_run(&mut runs, run("/a", 1));
        remember_run(&mut runs, run("/b", 2));
        remember_run(&mut runs, run("/a", 3));
        assert_eq!(
            runs.iter()
                .map(|run| (run.services[0].dir.as_str(), run.ran_at))
                .collect::<Vec<_>>(),
            vec![("/a", 3), ("/b", 2)]
        );
        for at in 0..20 {
            remember_run(&mut runs, run(&format!("/{at}"), at));
        }
        assert_eq!(runs.len(), MAX_RUNS);
    }

    #[test]
    fn remember_keeps_newest_first_deduped_and_capped() {
        let mut entry = TargetHistory::default();
        for command in ["a", "b", "c", "b", "d"] {
            remember(&mut entry, Some("/work".to_owned()), command.to_owned());
        }
        assert_eq!(entry.commands, ["d", "b", "c"], "b moved up, a fell off");
        assert_eq!(entry.dir.as_deref(), Some("/work"));
    }

    #[test]
    fn keys_carry_cluster_namespace_and_path() {
        let target = TargetSpec::Path {
            path: "pod/zoo-pod".to_owned(),
            namespace: Some("tui-zoo".to_owned()),
        };
        assert_eq!(keyed("bearkube", &target), "bearkube/tui-zoo/pod/zoo-pod");
        assert_eq!(
            keyed("bearkube", &TargetSpec::Targetless),
            "bearkube/targetless"
        );
    }
}
