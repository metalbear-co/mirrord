//! The code sync settings of a preview session, and the checks that the CLI and the operator both
//! run on them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
mod tests;

/// The paths at which the operator mounts its own volumes into a preview pod that syncs code. No
/// `remote` or `install.persist` path may lie under one, because the two mounts would hide each
/// other.
pub const RESERVED_SYNC_PATHS: [&str; 4] = [
    "/mirrord",
    "/mirrord-state",
    "/mirrord-code",
    "/mirrord-sync-tls",
];

/// Code sync settings of a preview session.
///
/// The session runs its image, or the target's when `spec.image` is empty, with `run_command` on
/// the code that the CLI pushes into the `remote` of each entry.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncConfig {
    /// The directories that the CLI pushes into the preview pod. A push names each entry by its
    /// index in this list.
    pub entries: Vec<PreviewSyncEntry>,

    /// The command that runs the app in the target container, as a list of the program and its
    /// arguments. It replaces the container's command and arguments.
    pub run_command: Vec<String>,

    /// The absolute working directory of `run_command` and of the install command in the target
    /// container. `None` keeps the container's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// The command that installs the app's dependencies, if the app needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<PreviewSyncInstall>,

    /// Whether the app restarts after each push. A dev server that reloads changed files on its
    /// own doesn't need the restart.
    #[serde(default = "default_restart_on_change")]
    pub restart_on_change: bool,
}

fn default_restart_on_change() -> bool {
    true
}

/// One directory that the CLI pushes into the preview pod.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncEntry {
    /// The absolute path in the target container that holds the pushed tree.
    pub remote: String,
}

/// The command that installs the app's dependencies in the preview pod.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncInstall {
    /// The command, as a list of the program and its arguments. It runs before `run_command`
    /// starts, and again after a push that changes a file that `on_change` matches.
    pub command: Vec<String>,

    /// The files whose changes run `command` again, as absolute globs in the target container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_change: Vec<String>,

    /// The absolute paths outside every entry's `remote` that `command` writes to. The preview
    /// keeps them on its code volume, seeded from the image, so the installed files survive a
    /// restart of the pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persist: Vec<String>,
}

/// A problem with [`PreviewSyncConfig`] that keeps the preview from syncing code. The messages
/// name the options of `mirrord-up.yaml`, which the settings come from.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PreviewSyncConfigError {
    #[error("`sync` has no entries")]
    NoEntries,

    #[error("`run.command` is empty")]
    EmptyRunCommand,

    #[error("`install.command` is empty")]
    EmptyInstallCommand,

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
    /// one entry, the commands aren't empty, `working_dir`, the `on_change` globs and the mount
    /// paths are absolute and normalized, and each `remote` and `install.persist` path is a
    /// separate mount that isn't `/`, doesn't lie under a [`RESERVED_SYNC_PATHS`] path, and
    /// doesn't equal, contain or lie inside another one.
    pub fn check(&self) -> Result<(), PreviewSyncConfigError> {
        if self.entries.is_empty() {
            return Err(PreviewSyncConfigError::NoEntries);
        }

        if self.run_command.is_empty() {
            return Err(PreviewSyncConfigError::EmptyRunCommand);
        }

        if let Some(working_dir) = &self.working_dir {
            components("run.remote_directory", working_dir)?;
        }

        if let Some(install) = &self.install
            && install.command.is_empty()
        {
            return Err(PreviewSyncConfigError::EmptyInstallCommand);
        }

        let mounts = self
            .entries
            .iter()
            .map(|entry| ("sync[].remote", entry.remote.as_str()))
            .chain(
                self.install
                    .iter()
                    .flat_map(|install| &install.persist)
                    .map(|path| ("install.persist", path.as_str())),
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
        for glob in self.install.iter().flat_map(|install| &install.on_change) {
            components("install.on_change", glob)?;
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
