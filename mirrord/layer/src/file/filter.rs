/// Controls which files are ignored (open local) by mirrord file operations.
///
/// There are 3 ways of setting this up:
///
/// 1. no configuration (default): will bypass file operations for file paths and types that
/// match [`generate_local_set`];
///
/// 2. Using the overrides for `read_only`, `read_write` and `local`.
use std::{env, sync::OnceLock};

use mirrord_config::{
    fs::{FsConfig, FsModeConfig},
    util::VecOrSingle,
};
use regex::{RegexSet, RegexSetBuilder};
use tracing::warn;

use crate::detour::{Bypass, Detour};

/// Shortcut for checking an optional regex set.
/// Usage is `some_regex_match(Option<RegexSet>, text)`
macro_rules! some_regex_match {
    ($regex:expr, $text:expr) => {
        $regex
            .as_ref()
            .map(|__regex| __regex.is_match($text))
            .unwrap_or_default()
    };
}

/// List of files that mirrord should use locally, as they probably exist only in the local user
/// machine, or are system configuration files (that could break the process if we used the remote
/// version).
///
/// You most likely do **NOT** want to include any of these, but if have a reason to do so, then
/// setting any of the overrides - `MIRRORD_FILE_X_PATTERN` allows you to override this list.
fn generate_local_set() -> RegexSet {
    // To handle the problem of injecting `open` and friends into project runners (like in a call to
    // `node app.js`, or `cargo run app`), we're ignoring files from the current working directory.
    let current_dir = env::current_dir().unwrap();
    let current_binary = env::current_exe().unwrap();
    let temp_dir = env::temp_dir();
    let patterns = [
        r"^.+\.so$",
        r"^.+\.d$",
        r"^.+\.pyc$",
        r"^.+\.py$",
        r"^.+\.js$",
        r"^.+\.pth$",
        r"^.+\.plist$",
        r"^.*venv\.cfg$",
        r"^/proc/.*$",
        r"^/sys/.*$",
        r"^/lib/.*$",
        r"^/etc/.*$",
        r"^/usr/.*$",
        r"^/bin/.*$",
        r"^/sbin/.*$",
        r"^/dev/.*$",
        r"^/opt/.*$",
        r"^/home/.*$",
        r"^/tmp(/|$)",
        r"^/snap/.*$",
        // support for nixOS.
        r"^/nix/.*$",
        r".+\.asdf/.+",
        r"^/home/iojs/.*$",
        r"^/home/runner/.*$",
        // dotnet: `/tmp/clr-debug-pipe-1`
        r"^.*clr-.*-pipe-.*$",
        // dotnet: `/home/{username}/{project}.pdb`
        r"^.*\.pdb$",
        // dotnet: `/home/{username}/{project}.dll`
        r"^.*\.dll$",
        // jvm.cfg or ANYTHING/jvm.cfg
        r".*(^|/)jvm\.cfg$",
        // TODO: `node` searches for this file in multiple directories, bypassing some of our
        // ignore regexes, maybe other "project runners" will do the same.
        r"^.*/package.json$",
        // asdf
        r".*/\.tool-versions$",
        // macOS
        #[cfg(target_os = "macos")]
        "^/Volumes(/|$)",
        #[cfg(target_os = "macos")]
        r"^/private(/|$)",
        #[cfg(target_os = "macos")]
        r"^/var/folders(/|$)",
        #[cfg(target_os = "macos")]
        r"^/Users",
        #[cfg(target_os = "macos")]
        r"^/Library",
        #[cfg(target_os = "macos")]
        r"^/Applications",
        #[cfg(target_os = "macos")]
        r"^/System",
        #[cfg(target_os = "macos")]
        r"^/var/run/com.apple",
        &format!("^{}", temp_dir.to_string_lossy()),
        &format!("^.*{}.*$", current_dir.to_string_lossy()),
        &format!("^.*{}.*$", current_binary.to_string_lossy()),
        "^/$", // root
    ];
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building local path regex set failed")
}

/// List of files that mirrord should use remotely read only
fn generate_remote_ro_set() -> RegexSet {
    let patterns = [
        // AWS cli cache
        // "file not exist" for identity caches (AWS)
        r".aws/cli/cache/.+\.json$",
        r".aws/credentials$",
        r".aws/config$",
        // for dns resolving
        r"^/etc/resolv.conf$",
        r"^/etc/hosts$",
        r"^/etc/hostname$",
    ];
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building local path regex set failed")
}

/// Global filter used by file operations to bypass (use local) or continue (use remote).
pub(crate) static FILE_FILTER: OnceLock<FileFilter> = OnceLock::new();

pub(crate) struct FileFilter {
    read_only: Option<RegexSet>,
    read_write: Option<RegexSet>,
    local: Option<RegexSet>,
    default_local: RegexSet,
    default_remote_ro: RegexSet,
    mode: FsModeConfig,
}

/// Builds case insensitive regexes from a list of patterns.
/// Returns `None` if the list is empty.
/// Regex error if fails.
fn build_regex_or_none(patterns: Vec<String>) -> Result<Option<RegexSet>, regex::Error> {
    if patterns.is_empty() {
        Ok(None)
    } else {
        RegexSetBuilder::new(patterns)
            .case_insensitive(true)
            .build()
            .map(Some)
    }
}

impl FileFilter {
    /// Initializes a `FileFilter` based on the user configuration.
    ///
    /// The filter first checks if the user specified any include/exclude regexes. (This will be
    /// removed) If path matches include, it continues to check if the path has specific
    /// behavior, if not, it checks if the path matches the default exclude list.
    /// If not, it does the default behavior set by user (default is read only remote).
    #[tracing::instrument(level = "trace")]
    pub(crate) fn new(fs_config: FsConfig) -> Self {
        let FsConfig {
            read_write,
            read_only,
            local,
            mode,
        } = fs_config;

        let read_write =
            build_regex_or_none(read_write.map(VecOrSingle::to_vec).unwrap_or_default())
                .expect("Building read-write regex set failed");
        let read_only = build_regex_or_none(read_only.map(VecOrSingle::to_vec).unwrap_or_default())
            .expect("Building read-only regex set failed");
        let local = build_regex_or_none(local.map(VecOrSingle::to_vec).unwrap_or_default())
            .expect("Building local path regex set failed");

        let default_local = generate_local_set();
        let default_remote_ro = generate_remote_ro_set();

        Self {
            read_only,
            read_write,
            local,
            default_local,
            default_remote_ro,
            mode,
        }
    }

    /// Checks if `text` matches the regex held by the initialized variant of `FileFilter`,
    /// and the whether the path is queried for write converting the result a `Detour`.
    ///
    /// `op` is used to lazily initialize a `Bypass` case.
    pub(crate) fn continue_or_bypass_with<F>(&self, text: &str, write: bool, op: F) -> Detour<()>
    where
        F: FnOnce() -> Bypass,
    {
        if matches!(&self.mode, FsModeConfig::Local) {
            return Detour::Bypass(op());
        }

        if some_regex_match!(self.read_write, text) {
            Detour::Success(())
        } else if some_regex_match!(self.read_only, text) {
            if !write {
                Detour::Success(())
            } else {
                Detour::Bypass(op())
            }
        } else if some_regex_match!(self.local, text) {
            Detour::Bypass(op())
        } else if self.default_remote_ro.is_match(text) && !write {
            Detour::Success(())
        } else if self.default_local.is_match(text) {
            Detour::Bypass(op())
        } else {
            match self.mode {
                FsModeConfig::Write => Detour::Success(()),
                FsModeConfig::Read if !write => Detour::Success(()),
                FsModeConfig::Read if write => Detour::Bypass(Bypass::ReadOnly(text.into())),
                _ => Detour::Bypass(op()),
            }
        }
    }
}

impl Default for FileFilter {
    fn default() -> Self {
        Self::new(FsConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use mirrord_config::{fs::FsConfig, util::VecOrSingle};
    use rstest::*;

    use super::*;
    use crate::detour::{Bypass, Detour};

    /// Implementation of helper methods for testing [`Detour`].
    impl<S> Detour<S> {
        /// Convenience function to convert [`Detour::Bypass`] to `bool`.
        fn is_bypass(&self) -> bool {
            matches!(self, Detour::Bypass(_))
        }
    }

    #[rstest]
    #[trace]
    #[case(FsModeConfig::Write, "/a/test.a", false, false)]
    #[case(FsModeConfig::Write, "/pain/read_write/test.a", false, false)]
    #[case(FsModeConfig::Write, "/pain/read_only/test.a", false, false)]
    #[case(FsModeConfig::Write, "/pain/write.a", false, false)]
    #[case(FsModeConfig::Write, "/pain/local/test.a", false, true)]
    #[case(FsModeConfig::Write, "/opt/test.a", false, true)]
    #[case(FsModeConfig::Write, "/a/test.a", true, false)]
    #[case(FsModeConfig::Write, "/pain/read_write/test.a", true, false)]
    #[case(FsModeConfig::Write, "/pain/read_only/test.a", true, true)]
    #[case(FsModeConfig::Write, "/pain/write.a", true, false)]
    #[case(FsModeConfig::Write, "/pain/local/test.a", true, true)]
    #[case(FsModeConfig::Write, "/opt/test.a", true, true)]
    #[case(FsModeConfig::Read, "/a/test.a", false, false)]
    #[case(FsModeConfig::Read, "/pain/read_write/test.a", false, false)]
    #[case(FsModeConfig::Read, "/pain/read_only/test.a", false, false)]
    #[case(FsModeConfig::Read, "/pain/write.a", false, false)]
    #[case(FsModeConfig::Read, "/pain/local/test.a", false, true)]
    #[case(FsModeConfig::Read, "/opt/test.a", false, true)]
    #[case(FsModeConfig::Read, "/a/test.a", true, true)]
    #[case(FsModeConfig::Read, "/pain/read_write/test.a", true, false)]
    #[case(FsModeConfig::Read, "/pain/read_only/test.a", true, true)]
    #[case(FsModeConfig::Read, "/pain/write.a", true, true)]
    #[case(FsModeConfig::Read, "/pain/local/test.a", true, true)]
    #[case(FsModeConfig::Read, "/opt/test.a", true, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/a/test.a", false, true)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_write/test.a",
        false,
        false
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_only/test.a",
        false,
        false
    )]
    #[case(FsModeConfig::LocalWithOverrides, "/pain/write.a", false, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/pain/local/test.a", false, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/opt/test.a", false, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/a/test.a", true, true)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_write/test.a",
        true,
        false
    )]
    #[case(FsModeConfig::LocalWithOverrides, "/pain/read_only/test.a", true, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/pain/write.a", true, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/pain/local/test.a", true, true)]
    #[case(FsModeConfig::LocalWithOverrides, "/opt/test.a", true, true)]
    #[case(FsModeConfig::Read, "/usr/a/.aws/cli/cache/121.json", true, true)]
    #[case(FsModeConfig::Write, "/usr/a/.aws/cli/cache/121.json", true, true)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/usr/a/.aws/cli/cache/121.json",
        true,
        true
    )]
    #[case(FsModeConfig::Local, "/a/test.a", false, true)]
    #[case(FsModeConfig::Local, "/pain/read_write/test.a", false, true)]
    #[case(FsModeConfig::Local, "/pain/read_only/test.a", false, true)]
    #[case(FsModeConfig::Local, "/pain/write.a", false, true)]
    #[case(FsModeConfig::Local, "/pain/local/test.a", false, true)]
    #[case(FsModeConfig::Local, "/opt/test.a", false, true)]
    #[case(FsModeConfig::Local, "/a/test.a", true, true)]
    #[case(FsModeConfig::Local, "/pain/read_write/test.a", true, true)]
    #[case(FsModeConfig::Local, "/pain/read_only/test.a", true, true)]
    #[case(FsModeConfig::Local, "/pain/write.a", true, true)]
    #[case(FsModeConfig::Local, "/pain/local/test.a", true, true)]
    #[case(FsModeConfig::Local, "/opt/test.a", true, true)]
    fn include_complex_configuration(
        #[case] mode: FsModeConfig,
        #[case] path: &str,
        #[case] write: bool,
        #[case] bypass: bool,
    ) {
        let read_write = Some(VecOrSingle::Multiple(vec![
            r"/pain/read_write.*\.a".to_string()
        ]));
        let read_only = Some(VecOrSingle::Multiple(vec![
            r"/pain/read_only.*\.a".to_string()
        ]));
        let local = Some(VecOrSingle::Multiple(vec![r"/pain/local.*\.a".to_string()]));
        let fs_config = FsConfig {
            read_write,
            read_only,
            local,
            mode,
        };

        let file_filter = FileFilter::new(fs_config);

        assert_eq!(
            file_filter
                .continue_or_bypass_with(path, write, || Bypass::IgnoredFile("".into()))
                .is_bypass(),
            bypass
        );
    }

    #[rstest]
    #[case(FsModeConfig::Read, "/usr/a/.aws/cli/cache/121.json", true, true)]
    #[case(FsModeConfig::Write, "/usr/a/.aws/cli/cache/121.json", true, true)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/usr/a/.aws/cli/cache/121.json",
        true,
        true
    )]
    #[case(FsModeConfig::Read, "/usr/a/.aws/cli/cache/121.json", false, false)]
    #[case(FsModeConfig::Write, "/usr/a/.aws/cli/cache/121.json", false, false)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/usr/a/.aws/cli/cache/1241.json",
        false,
        false
    )]
    fn remote_read_only_set(
        #[case] mode: FsModeConfig,
        #[case] path: &str,
        #[case] write: bool,
        #[case] bypass: bool,
    ) {
        let fs_config = FsConfig {
            mode,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        assert_eq!(
            file_filter
                .continue_or_bypass_with(path, write, || Bypass::IgnoredFile("".into()))
                .is_bypass(),
            bypass
        );
    }
}
