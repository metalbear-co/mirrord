/// Controls which files are ignored (open local) by mirrord file operations.
///
/// There are 3 ways of setting this up:
///
/// 1. no configuration (default): will bypass file operations for file paths and types that
/// match [`DEFAULT_EXCLUDE_LIST`];
///
/// 2. `MIRRORD_FILE_FILTER_INCLUDE`: includes only the files specified by the user (input)
/// regex;
///
/// 3. `MIRRORD_FILE_FILTER_EXCLUDE`: similar to the default, it includes everything that is
/// not filtered by the user (input) regex, and [`DEFAULT_EXCLUDE_LIST`];
use std::{
    env,
    sync::{LazyLock, OnceLock},
};

use fancy_regex::Regex;
use mirrord_config::{fs::FsConfig, util::VecOrSingle};
use tracing::warn;

use crate::detour::{Bypass, Detour};

/// List of files that mirrord should ignore, as they probably exist only in the local user machine,
/// or are system configuration files (that could break the process if we used the remote version).
///
/// You most likely do **NOT** want to include any of these, but if have a reason to do so, then
/// setting `MIRRORD_FILE_FILTER_INCLUDE` allows you to override this list.
static DEFAULT_EXCLUDE_LIST: LazyLock<String> = LazyLock::new(|| {
    // To handle the problem of injecting `open` and friends into project runners (like in a call to
    // `node app.js`, or `cargo run app`), we're ignoring files from the current working directory.
    let current_dir = env::current_dir().unwrap();
    let current_binary = env::current_exe().unwrap();
    [
        r"(?<so_files>^.+\.so$)|",
        r"(?<d_files>^.+\.d$)|",
        r"(?<pyc_files>^.+\.pyc$)|",
        r"(?<py_files>^.+\.py$)|",
        r"(?<js_files>^.+\.js$)|",
        r"(?<pth_files>^.+\.pth$)|",
        r"(?<plist_files>^.+\.plist$)|",
        r"(?<cfg_files>^.*venv\.cfg$)|",
        r"(?<proc_path>^/proc/.*$)|",
        r"(?<sys_path>^/sys/.*$)|",
        r"(?<lib_path>^/lib/.*$)|",
        r"(?<etc_path>^/etc/.*$)|",
        r"(?<usr_path>^/usr/.*$)|",
        r"(?<dev_path>^/dev/.*$)|",
        r"(?<opt_path>^/opt/.*$)|",
        r"(?<home_path>^/home/.*$)|",
        // support for nixOS.
        r"(?<nix_path>^/nix/.*$)|",
        r"(?<iojs_path>^/home/iojs/.*$)|",
        r"(?<runner_path>^/home/runner/.*$)|",
        // dotnet: `/tmp/clr-debug-pipe-1`
        r"(?<clr_files>^.*clr-.*-pipe-.*$)|",
        // dotnet: `/home/{username}/{project}.pdb`
        r"(?<pdb_files>^.*\.pdb$)|",
        // dotnet: `/home/{username}/{project}.dll`
        r"(?<dll_files>^.*\.dll$)|",
        // jvm.cfg or ANYTHING/jvm.cfg
        r"(?<jvm_files>.*(^|/)jvm\.cfg$)|",
        // TODO: `node` searches for this file in multiple directories, bypassing some of our
        // ignore regexes, maybe other "project runners" will do the same.
        r"(?<package_json>^.*/package.json$)|",
        // macOS
        r"(?<macos_users>^/Users/.*$)|",
        r"(?<macos_library>^/Library/.*$)|",
        &format!("(^.*{}.*$)|", current_dir.to_string_lossy()),
        &format!("(^.*{}.*$)", current_binary.to_string_lossy()),
    ]
    .into_iter()
    .collect()
});

/// Global filter used by file operations to bypass (use local) or continue (use remote).
pub(crate) static FILE_FILTER: OnceLock<FileFilter> = OnceLock::new();

/// Holds the `Regex` that is used to either continue or bypass file path operations (such as
/// [`file::ops::open`]), according to what the user specified.
///
/// The [`FileFilter::Include`] variant takes precedence and erases whatever the user supplied as
/// exclude, this means that if the user specifies both, `FileFilter::Exclude` is never constructed.
///
/// Warning: Use [`FileFilter::new`] (or equivalent) when initializing this, otherwise the above
/// constraint might not be held.
#[derive(Debug, Clone)]
pub(crate) enum FileFilter {
    /// User specified `Regex` containing the file paths that the user wants to include for file
    /// operations.
    ///
    /// Overrides [`FileFilter::Exclude`].
    Include(Regex),

    /// Combination of [`DEFAULT_EXCLUDE_LIST`] and the user's specified `Regex`.
    ///
    /// Anything not matched by this `Regex` is considered as included.
    Exclude(Regex),
}

impl FileFilter {
    /// Initializes a `FileFilter` based on the user configuration.
    ///
    /// - [`FileFilter::Include`] is returned if the user specified any include path (thus erasing
    ///   anything passed as exclude);
    /// - [`FileFilter::Exclude`] also appends the [`DEFAULT_EXCLUDE_LIST`] to the user supplied
    ///   regex;
    #[tracing::instrument(level = "trace")]
    pub(crate) fn new(fs_config: FsConfig) -> Self {
        let FsConfig {
            include, exclude, ..
        } = fs_config;

        let include = include.map(VecOrSingle::to_vec).unwrap_or_default();
        let exclude = exclude.map(VecOrSingle::to_vec).unwrap_or_default();

        let default_exclude = DEFAULT_EXCLUDE_LIST.as_ref();
        let default_exclude_regex =
            Regex::new(default_exclude).expect("Failed parsing default exclude file regex!");

        // Converts a list of `String` into one big regex-fied `String`.
        let reduce_to_string = |list: Vec<String>| {
            list.into_iter()
                // Turn into capture group `(/folder/first.txt)`.
                .map(|element| format!("({element})"))
                // Put `or` operation between groups `(/folder/first.txt)|(/folder/second.txt)`.
                .reduce(|acc, element| format!("{acc}|{element}"))
        };

        let exclude = reduce_to_string(exclude)
            // Add default exclude list.
            .map(|mut user_exclude| {
                user_exclude.push_str(&format!("|{default_exclude}"));
                user_exclude
            })
            .as_deref()
            .map(Regex::new)
            .transpose()
            .expect("Failed parsing exclude file regex!")
            // `exclude` always exists, either as user input, or as our default exclude list.
            .unwrap_or(default_exclude_regex);

        // Try to generate the final `Regex` based on `include`.
        reduce_to_string(include)
            .as_deref()
            .map(Regex::new)
            .transpose()
            .expect("Failed parsing include file regex!")
            .map(Self::Include)
            // `include` was empty, so we fallback to `exclude`.
            .unwrap_or(Self::Exclude(exclude))
    }

    // TODO(alex) [low] 2022-11-17: This could take `(op: (T) -> U)` to be generic enough to be in
    // the `Detour` implementation.
    /// Checks if `text` matches the regex held by the initialized variant of `FileFilter`,
    /// converting the result a `Detour`.
    ///
    /// `op` is used to lazily initialize a `Bypass` case.
    pub(crate) fn continue_or_bypass_with<F>(&self, text: &str, op: F) -> Detour<()>
    where
        F: FnOnce() -> Bypass,
    {
        // Order matters here, as we want to make `include` the most important pattern. If the user
        // specified `include`, then we never want to accidentally allow other paths to pass this
        // check (this is a corner case that is unlikely to happen, as initialization via
        // `FileFilter::new` should prevent it from ever seeing the light of day).
        match self {
            FileFilter::Include(include) if include.is_match(text).unwrap() => Detour::Success(()),
            FileFilter::Exclude(exclude) if !exclude.is_match(text).unwrap() => Detour::Success(()),
            _ => Detour::Bypass(op()),
        }
    }
}

impl Default for FileFilter {
    fn default() -> Self {
        Self::Exclude(Regex::new(&DEFAULT_EXCLUDE_LIST).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use mirrord_config::{fs::FsConfig, util::VecOrSingle};

    use super::FileFilter;
    use crate::detour::{Bypass, Detour};

    /// Implementation of helper methods for testing [`Detour`].
    impl<S> Detour<S> {
        /// Convenience function to convert [`Detour::Success`] to `bool`.
        fn is_success(&self) -> bool {
            matches!(self, Detour::Success(_))
        }

        /// Convenience function to convert [`Detour::Bypass`] to `bool`.
        fn is_bypass(&self) -> bool {
            matches!(self, Detour::Bypass(_))
        }
    }

    #[test]
    fn test_include_only_filter() {
        let include = Some(VecOrSingle::Multiple(vec![
            "/folder/first.a".to_string(),
            "/folder/second.a".to_string(),
        ]));

        let fs_config = FsConfig {
            include,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        assert!(file_filter
            .continue_or_bypass_with("/folder/first.a", || Bypass::IgnoredFile("first.a".into()))
            .is_success());

        assert!(file_filter
            .continue_or_bypass_with("/folder/second.a", || Bypass::IgnoredFile(
                "second.a".into()
            ))
            .is_success());

        assert!(file_filter
            .continue_or_bypass_with("/folder/third.a", || Bypass::IgnoredFile("third.a".into()))
            .is_bypass());
    }

    #[test]
    fn test_exclude_only_filter() {
        let exclude = Some(VecOrSingle::Multiple(vec![
            "/folder/first.a".to_string(),
            "/folder/second.a".to_string(),
        ]));

        let fs_config = FsConfig {
            exclude,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        assert!(file_filter
            .continue_or_bypass_with("/folder/first.a", || Bypass::IgnoredFile("first.a".into()))
            .is_bypass());

        assert!(file_filter
            .continue_or_bypass_with("/folder/second.a", || Bypass::IgnoredFile(
                "second.a".into()
            ))
            .is_bypass());

        assert!(file_filter
            .continue_or_bypass_with("/folder/third.a", || Bypass::IgnoredFile("third.a".into()))
            .is_success());
    }

    #[test]
    fn test_include_overrides_exclude() {
        let include = Some(VecOrSingle::Multiple(vec![
            "/folder/first.a".to_string(),
            "/folder/second.a".to_string(),
        ]));

        let exclude = include.clone();

        let fs_config = FsConfig {
            include,
            exclude,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        assert!(file_filter
            .continue_or_bypass_with("/folder/first.a", || Bypass::IgnoredFile("first.a".into()))
            .is_success());

        assert!(file_filter
            .continue_or_bypass_with("/folder/second.a", || Bypass::IgnoredFile(
                "second.a".into()
            ))
            .is_success());

        assert!(file_filter
            .continue_or_bypass_with("/folder/third.a", || Bypass::IgnoredFile("third.a".into()))
            .is_bypass());
    }

    #[test]
    fn test_regex_include_only_filter() {
        let include = Some(VecOrSingle::Multiple(vec![r"/folder/.*\.a".to_string()]));

        let fs_config = FsConfig {
            include,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        assert!(file_filter
            .continue_or_bypass_with("/folder/first.a", || Bypass::IgnoredFile("first.a".into()))
            .is_success());

        assert!(file_filter
            .continue_or_bypass_with("/folder/second.a", || Bypass::IgnoredFile(
                "second.a".into()
            ))
            .is_success());
    }

    #[test]
    fn test_regex_exclude_only_filter() {
        let exclude = Some(VecOrSingle::Multiple(vec![r"/folder/.*\.a".to_string()]));

        let fs_config = FsConfig {
            exclude,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        assert!(file_filter
            .continue_or_bypass_with("/folder/first.a", || Bypass::IgnoredFile("first.a".into()))
            .is_bypass());

        assert!(file_filter
            .continue_or_bypass_with("/folder/second.a", || Bypass::IgnoredFile(
                "second.a".into()
            ))
            .is_bypass());

        assert!(file_filter
            .continue_or_bypass_with("/dir/third.a", || Bypass::IgnoredFile("second.a".into()))
            .is_success());
    }
}
