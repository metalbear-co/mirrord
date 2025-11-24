use std::path::Path;

/// Controls which files are ignored (opened locally) by mirrord file operations.
///
/// There are 2 ways of setting this up:
///
/// 1. no configuration (default): will bypass file operations for file paths and types that
///    match [`generate_local_set`];
///
/// 2. Using the overrides for `read_only`, `read_write` and `local`.
use mirrord_config::{
    feature::fs::{FsConfig, FsModeConfig},
    util::VecOrSingle,
};
use regex::{RegexSet, RegexSetBuilder};

#[cfg(unix)]
use super::unix::*;
#[cfg(windows)]
use super::windows::*;
use crate::util::get_home_path;

/// List of files that mirrord should use locally, as they probably exist only in the local user
/// machine, or are system configuration files (that could break the process if we used the remote
/// version).
///
/// You most likely do **NOT** want to include any of these, but if have a reason to do so, then
/// setting any of the overrides - `MIRRORD_FILE_X_PATTERN` allows you to override this list.
pub fn generate_local_set() -> RegexSet {
    read_local_by_default::regex_set_builder()
        .case_insensitive(true)
        .build()
        .expect("Building local path regex set failed")
}

/// List of files that mirrord should use remotely read only
pub fn generate_remote_ro_set() -> RegexSet {
    let patterns = read_remote_by_default::PATHS;
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building remote readonly path regex set failed")
}

pub fn generate_not_found_set() -> RegexSet {
    let Some(home_clean) = get_home_path().map(|x| x.to_string_lossy().into_owned()) else {
        tracing::warn!("Unable to resolve home directory, generating empty not-found set");
        return Default::default();
    };

    let patterns = not_found_by_default::PATHS
        .into_iter()
        .map(|cloud_dir| format!("^{home_clean}/{cloud_dir}"));

    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building not found path regex set failed")
}

#[derive(Debug, PartialEq)]
pub enum FileMode {
    /// `bool` is for whether the decision for this file mode to be local
    /// is from defaults.
    Local(bool),
    /// `bool` is for whether the decision for this file mode to be not found
    /// is from defaults.
    NotFound(bool),
    /// `bool` is for whether the decision for this file mode to be read only
    /// is from defaults.
    ReadOnly(bool),
    /// `bool` is for whether the decision for this file mode to be read write
    /// is from defaults.
    ReadWrite(bool),
}

#[derive(Debug)]
pub struct FileFilter {
    pub read_only: RegexSet,
    pub read_write: RegexSet,
    pub local: RegexSet,
    pub not_found: RegexSet,
    pub default_local: RegexSet,
    pub default_remote_ro: RegexSet,
    pub default_not_found: RegexSet,
    pub mode: FsModeConfig,
}

impl FileFilter {
    pub fn make_regex_set(patterns: Option<VecOrSingle<String>>) -> Result<RegexSet, regex::Error> {
        RegexSetBuilder::new(patterns.as_deref().map(<[_]>::to_vec).unwrap_or_default())
            .case_insensitive(true)
            .build()
    }

    /// Initializes a `FileFilter` based on the user configuration.
    ///
    /// The filter first checks if the user specified any include/exclude regexes. (This will be
    /// removed) If path matches include, it continues to check if the path has specific
    /// behavior, if not, it checks if the path matches the default exclude list.
    /// If not, it does the default behavior set by user (default is read only remote).
    #[mirrord_layer_macro::instrument(level = "trace")]
    pub fn new(fs_config: FsConfig) -> Self {
        let FsConfig {
            read_write,
            read_only,
            local,
            mode,
            not_found,
            ..
        } = fs_config;

        let read_write =
            Self::make_regex_set(read_write).expect("building read-write regex set failed");
        let read_only =
            Self::make_regex_set(read_only).expect("building read-only regex set failed");
        let local = Self::make_regex_set(local).expect("building local path regex set failed");
        let not_found =
            Self::make_regex_set(not_found).expect("building not-found regex set failed");

        let default_local = generate_local_set();
        let default_remote_ro = generate_remote_ro_set();
        let default_not_found = generate_not_found_set();

        Self {
            read_only,
            read_write,
            local,
            not_found,
            default_local,
            default_remote_ro,
            default_not_found,
            mode,
        }
    }

    pub fn check<T: AsRef<str>>(&self, path: T) -> Option<FileMode> {
        let path = path.as_ref();

        match self.mode {
            FsModeConfig::Local => Some(FileMode::Local(false)),
            FsModeConfig::Read | FsModeConfig::Write | FsModeConfig::LocalWithOverrides => {
                if self.not_found.is_match(path) {
                    Some(FileMode::NotFound(false))
                } else if self.read_write.is_match(path) {
                    Some(FileMode::ReadWrite(false))
                } else if self.read_only.is_match(path) {
                    Some(FileMode::ReadOnly(false))
                } else if self.local.is_match(path) {
                    Some(FileMode::Local(false))
                } else if self.default_not_found.is_match(path) {
                    Some(FileMode::NotFound(true))
                } else if self.default_remote_ro.is_match(path) {
                    Some(FileMode::ReadOnly(true))
                } else if self.default_local.is_match(path) {
                    Some(FileMode::Local(true))
                } else {
                    None
                }
            }
        }
    }

    pub fn check_not_found(&self, path: &Path) -> bool {
        matches!(
            self.check(path.to_str().unwrap_or_default()),
            Some(FileMode::NotFound(_))
        )
    }
}

impl Default for FileFilter {
    fn default() -> Self {
        Self::new(FsConfig::default())
    }
}
