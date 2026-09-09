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
fn cluster() -> String {
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
            .and_then(|text| serde_yaml::from_str(&text).ok())
            .unwrap_or_default();
        RwLock::new(loaded)
    })
}

/// The remembered settings for a target, if it ever ran.
pub fn recall(key: &str) -> Option<TargetHistory> {
    store().read().ok()?.targets.get(key).cloned()
}

/// Records one launched service and persists the history.
pub fn record(key: String, dir: Option<String>, command: String) {
    let Ok(mut history) = store().write() else {
        return;
    };
    remember(history.targets.entry(key).or_default(), dir, command);

    let Some(path) = file() else { return };
    if let Some(parent) = path.parent() {
        _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_yaml::to_string(&*history) {
        _ = std::fs::write(path, text);
    }
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
