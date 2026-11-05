//! The code sync settings of a preview session, and the checks that the CLI and the operator both
//! run on them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
mod tests;

/// The paths at which the operator mounts its own volumes into a preview pod that syncs code. No
/// `remote` or `prepare.persist` path may lie under one, because the two mounts would hide each
/// other.
pub const RESERVED_SYNC_PATHS: [&str; 4] = [
    "/mirrord",
    "/mirrord-state",
    "/mirrord-code",
    "/mirrord-sync-tls",
];

/// Code sync settings of a preview session.
///
/// The session runs its image, or the target's when `spec.image` is empty, on the code that the
/// CLI syncs into the `remote` of each entry. With `run`, the operator's supervisor runs the app
/// with `run.command` in place of the container's command. Without it, the container keeps its
/// own command, and reads the synced files itself, as nginx does.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncConfig {
    /// The directories that the CLI syncs into the preview pod. A sync names each entry by its
    /// index in this list.
    pub entries: Vec<PreviewSyncEntry>,

    /// The command that prepares the code for the app, such as an install of its dependencies.
    /// Requires `run`, because the supervisor runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare: Option<PreviewSyncPrepare>,

    /// How the supervisor runs the app, or `None` to keep the container's own command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<PreviewSyncRun>,
}

/// One directory that the CLI syncs into the preview pod.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncEntry {
    /// The absolute path in the target container that holds the synced tree.
    pub remote: String,
}

/// How the supervisor runs the app in the preview pod.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncRun {
    /// The command that runs the app in the target container, as a list of the program and its
    /// arguments. It replaces the container's command and arguments.
    pub command: Vec<String>,

    /// The absolute working directory of `command` in the target container. `None` keeps the
    /// container's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// When the supervisor restarts the app.
    #[serde(default)]
    pub restart: PreviewSyncRestart,
}

/// When the supervisor restarts the app. A successful `prepare` always restarts it, because the
/// app must run on what `prepare` wrote.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PreviewSyncRestart {
    /// Only after `prepare` runs, for a dev server that reloads changed files on its own.
    #[default]
    OnPrepare,

    /// After every sync.
    OnSync,

    /// A value that a newer CLI sends, which this version doesn't know. The operator restarts the
    /// app as with [`PreviewSyncRestart::OnPrepare`].
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

/// The command that prepares the code for the app in the preview pod.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncPrepare {
    /// The command, as a list of the program and its arguments. It runs before `run.command`
    /// starts, and again after a sync that changes a file that `triggeredBy` matches.
    pub command: Vec<String>,

    /// The files whose changes run `command` again, as absolute globs in the target container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggered_by: Vec<String>,

    /// The absolute paths outside every entry's `remote` that `command` writes to. The preview
    /// keeps them on its code volume, seeded from the image, so the files that `command` wrote
    /// survive a restart of the pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persist: Vec<String>,

    /// The absolute working directory of `command` in the target container. `None` keeps the
    /// container's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

/// A problem with [`PreviewSyncConfig`] that keeps the preview from syncing code. The messages
/// name the options of `mirrord-up.yaml`, which the settings come from.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PreviewSyncConfigError {
    #[error("`sync.files` has no entries")]
    NoEntries,

    #[error("`sync.run.command` is empty")]
    EmptyRunCommand,

    #[error("`sync.prepare.command` is empty")]
    EmptyPrepareCommand,

    #[error(
        "`sync.prepare` needs `sync.run`: without `sync.run`, the preview's container runs its own \
         command, and nothing in the pod runs `sync.prepare.command`"
    )]
    PrepareWithoutRun,

    #[error(
        "the `{option}` path `{path}` must be absolute, with no `.`, `..` or empty components and \
         no trailing `/`"
    )]
    NotNormalized { option: &'static str, path: String },

    #[error("the `{option}` path can't be `/`")]
    Root { option: &'static str },

    #[error(
        "the `{option}` path `{path}` lies under `{reserved}`, which the preview pod uses for code \
         sync"
    )]
    Reserved {
        option: &'static str,
        path: String,
        reserved: &'static str,
    },

    #[error(
        "the `{first_option}` path `{first}` and the `{second_option}` path `{second}` overlap, \
         because one of them equals or contains the other"
    )]
    Overlap {
        first_option: &'static str,
        first: String,
        second_option: &'static str,
        second: String,
    },
}

impl PreviewSyncConfig {
    /// Checks that the operator can build a preview pod from these settings: there is at least
    /// one entry, `prepare` comes with `run`, the commands aren't empty, the working directories,
    /// the `triggered_by` globs and the mount paths are absolute and normalized, and each
    /// `remote` and `prepare.persist` path is a separate mount that isn't `/`, doesn't lie under
    /// a [`RESERVED_SYNC_PATHS`] path, and doesn't equal, contain or lie inside another one.
    pub fn check(&self) -> Result<(), PreviewSyncConfigError> {
        if self.entries.is_empty() {
            return Err(PreviewSyncConfigError::NoEntries);
        }

        if self.prepare.is_some() && self.run.is_none() {
            return Err(PreviewSyncConfigError::PrepareWithoutRun);
        }

        if let Some(run) = &self.run {
            if run.command.is_empty() {
                return Err(PreviewSyncConfigError::EmptyRunCommand);
            }

            if let Some(working_dir) = &run.working_dir {
                components("sync.run.directory", working_dir)?;
            }
        }

        if let Some(prepare) = &self.prepare {
            if prepare.command.is_empty() {
                return Err(PreviewSyncConfigError::EmptyPrepareCommand);
            }

            if let Some(working_dir) = &prepare.working_dir {
                components("sync.prepare.directory", working_dir)?;
            }
        }

        let mounts = self
            .entries
            .iter()
            .map(|entry| ("sync.files[].remote", entry.remote.as_str()))
            .chain(
                self.prepare
                    .iter()
                    .flat_map(|prepare| &prepare.persist)
                    .map(|path| ("sync.prepare.persist", path.as_str())),
            );

        let mut checked: Vec<(&'static str, &str, Vec<&str>)> = Vec::new();

        for (option, path) in mounts {
            let path_components = components(option, path)?;

            if path_components.is_empty() {
                return Err(PreviewSyncConfigError::Root { option });
            }

            if let Some(reserved) = RESERVED_SYNC_PATHS.into_iter().find(|reserved| {
                path.strip_prefix(reserved)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            }) {
                return Err(PreviewSyncConfigError::Reserved {
                    option,
                    path: path.to_owned(),
                    reserved,
                });
            }

            if let Some((first_option, first, _)) =
                checked.iter().find(|(_, _, other_components)| {
                    other_components.starts_with(&path_components)
                        || path_components.starts_with(other_components)
                })
            {
                return Err(PreviewSyncConfigError::Overlap {
                    first_option,
                    first: (*first).to_owned(),
                    second_option: option,
                    second: path.to_owned(),
                });
            }

            checked.push((option, path, path_components));
        }

        // The mounts come first, so that a `remote` that isn't normalized is reported as such,
        // rather than as the glob that the CLI built under it.
        for glob in self
            .prepare
            .iter()
            .flat_map(|prepare| &prepare.triggered_by)
        {
            components("sync.prepare.triggered_by", glob)?;
        }

        Ok(())
    }
}

/// The components of the absolute container `path`, which is a path of the `option` setting.
/// Fails unless `path` is absolute and normalized. The root has no components.
///
/// Container paths always use `/`, whatever the platform of the CLI, so this doesn't use
/// [`std::path::Path`].
fn components<'a>(
    option: &'static str,
    path: &'a str,
) -> Result<Vec<&'a str>, PreviewSyncConfigError> {
    let not_normalized = || PreviewSyncConfigError::NotNormalized {
        option,
        path: path.to_owned(),
    };

    let relative = path.strip_prefix('/').ok_or_else(not_normalized)?;

    if relative.is_empty() {
        return Ok(Vec::new());
    }

    relative
        .split('/')
        .map(|component| match component {
            "" | "." | ".." => Err(not_normalized()),
            component => Ok(component),
        })
        .collect()
}
