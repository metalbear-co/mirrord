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
    feature::fs::{FsConfig, FsModeConfig},
    util::VecOrSingle,
};
use regex::{RegexSet, RegexSetBuilder};
use tracing::warn;

use crate::{
    detour::{Bypass, Detour},
    error::HookError,
};

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
        r"^/usr(/|$).*$",
        r"^/home(/|$).*$",
        r"^/bin/.*$",
        r"^/sbin/.*$",
        r"^/dev/.*$",
        r"^/opt(/|$)",
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
        // for dns resolving
        r"^/etc/resolv.conf$",
        r"^/etc/hosts$",
        r"^/etc/hostname$",
    ];
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building remote readonly path regex set failed")
}

fn generate_not_found_set() -> RegexSet {
    let home = env::var("HOME").expect("failed to resolve $HOME");
    let home_clean = home.trim_end_matches('/');

    let patterns = [r"\.aws", r"\.config/gcloud", r"\.kube", r"\.azure"]
        .into_iter()
        .map(|cloud_dir| format!("{home_clean}/{cloud_dir}"));

    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building not found path regex set failed")
}

/// Global filter used by file operations to bypass (use local) or continue (use remote).
pub(crate) static FILE_FILTER: OnceLock<FileFilter> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct FileFilter {
    read_only: RegexSet,
    read_write: RegexSet,
    local: RegexSet,
    not_found: RegexSet,
    default_local: RegexSet,
    default_remote_ro: RegexSet,
    default_not_found: RegexSet,
    mode: FsModeConfig,
}

impl FileFilter {
    fn make_regex_set(patterns: Option<VecOrSingle<String>>) -> Result<RegexSet, regex::Error> {
        RegexSetBuilder::new(patterns.map(VecOrSingle::to_vec).unwrap_or_default())
            .case_insensitive(true)
            .build()
    }

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
            not_found,
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

    /// Checks if `text` matches the regex held by the initialized variant of `FileFilter`,
    /// and the whether the path is queried for write converting the result a `Detour`.
    ///
    /// `op` is used to lazily initialize a `Bypass` case.
    pub(crate) fn continue_or_bypass_with<F>(&self, text: &str, write: bool, op: F) -> Detour<()>
    where
        F: FnOnce() -> Bypass,
    {
        match self.mode {
            FsModeConfig::Local => Detour::Bypass(op()),
            _ if self.not_found.is_match(text) => Detour::Error(HookError::FileNotFound),
            _ if self.read_write.is_match(text) => Detour::Success(()),
            _ if self.read_only.is_match(text) => {
                if write {
                    Detour::Bypass(op())
                } else {
                    Detour::Success(())
                }
            }
            _ if self.default_not_found.is_match(text) => Detour::Error(HookError::FileNotFound),
            _ if self.local.is_match(text) => Detour::Bypass(op()),
            _ if self.default_remote_ro.is_match(text) && !write => Detour::Success(()),
            _ if self.default_local.is_match(text) => Detour::Bypass(op()),
            FsModeConfig::LocalWithOverrides => Detour::Bypass(op()),
            FsModeConfig::Write => Detour::Success(()),
            FsModeConfig::Read if write => Detour::Bypass(Bypass::ReadOnly(text.into())),
            FsModeConfig::Read => Detour::Success(()),
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
    use mirrord_config::{feature::fs::FsConfig, util::VecOrSingle};
    use rstest::*;

    use super::*;
    use crate::detour::{Bypass, Detour};

    /// Helper type for testing [`FileFilter`] results.
    #[derive(PartialEq, Eq, Debug)]
    enum DetourKind {
        Bypass,
        Error,
        Success,
    }

    impl<S> Detour<S> {
        fn kind(&self) -> DetourKind {
            match self {
                Self::Bypass(..) => DetourKind::Bypass,
                Self::Error(..) => DetourKind::Error,
                Self::Success(..) => DetourKind::Success,
            }
        }
    }

    #[rstest]
    #[trace]
    #[case(FsModeConfig::Write, "/a/test.a", false, DetourKind::Success)]
    #[case(
        FsModeConfig::Write,
        "/pain/read_write/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::Write,
        "/pain/read_only/test.a",
        false,
        DetourKind::Success
    )]
    #[case(FsModeConfig::Write, "/pain/write.a", false, DetourKind::Success)]
    #[case(FsModeConfig::Write, "/pain/local/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/opt/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/a/test.a", true, DetourKind::Success)]
    #[case(
        FsModeConfig::Write,
        "/pain/read_write/test.a",
        true,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::Write,
        "/pain/read_only/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Write, "/pain/write.a", true, DetourKind::Success)]
    #[case(FsModeConfig::Write, "/pain/local/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/opt/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/a/test.a", false, DetourKind::Success)]
    #[case(
        FsModeConfig::Read,
        "/pain/read_write/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::Read,
        "/pain/read_only/test.a",
        false,
        DetourKind::Success
    )]
    #[case(FsModeConfig::Read, "/pain/write.a", false, DetourKind::Success)]
    #[case(FsModeConfig::Read, "/pain/local/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/opt/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/a/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Read,
        "/pain/read_write/test.a",
        true,
        DetourKind::Success
    )]
    #[case(FsModeConfig::Read, "/pain/read_only/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/pain/write.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/pain/local/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/opt/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/a/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_write/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_only/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/write.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/local/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/opt/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/a/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_write/test.a",
        true,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_only/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/write.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/local/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/opt/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Read, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/etc/resolv.conf",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Local, "/a/test.a", false, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Local,
        "/pain/read_write/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::Local,
        "/pain/read_only/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Local, "/pain/write.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/pain/local/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/opt/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/a/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Local,
        "/pain/read_write/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::Local,
        "/pain/read_only/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Local, "/pain/write.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/pain/local/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/opt/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Local,
        "/pain/not_found/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/not_found/test.a",
        false,
        DetourKind::Error
    )]
    #[case(FsModeConfig::Read, "/pain/not_found/test.a", false, DetourKind::Error)]
    #[case(
        FsModeConfig::Write,
        "/pain/not_found/test.a",
        false,
        DetourKind::Error
    )]
    fn include_complex_configuration(
        #[case] mode: FsModeConfig,
        #[case] path: &str,
        #[case] write: bool,
        #[case] expected: DetourKind,
    ) {
        let read_write = Some(VecOrSingle::Multiple(vec![
            r"/pain/read_write.*\.a".to_string()
        ]));
        let read_only = Some(VecOrSingle::Multiple(vec![
            r"/pain/read_only.*\.a".to_string()
        ]));
        let local = Some(VecOrSingle::Multiple(vec![r"/pain/local.*\.a".to_string()]));
        let not_found = Some(VecOrSingle::Single(r"/pain/not_found.*\.a".to_string()));
        let fs_config = FsConfig {
            read_write,
            read_only,
            local,
            not_found,
            mode,
        };

        let file_filter = FileFilter::new(fs_config);

        let res =
            file_filter.continue_or_bypass_with(path, write, || Bypass::IgnoredFile("".into()));
        println!("filter result: {res:?}");
        assert_eq!(res.kind(), expected);
    }

    #[rstest]
    #[case(FsModeConfig::Read, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/etc/resolv.conf",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Read, "/etc/resolv.conf", false, DetourKind::Success)]
    #[case(FsModeConfig::Write, "/etc/resolv.conf", false, DetourKind::Success)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/etc/resolv.conf",
        false,
        DetourKind::Success
    )]
    fn remote_read_only_set(
        #[case] mode: FsModeConfig,
        #[case] path: &str,
        #[case] write: bool,
        #[case] expected: DetourKind,
    ) {
        let fs_config = FsConfig {
            mode,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        let res =
            file_filter.continue_or_bypass_with(path, write, || Bypass::IgnoredFile("".into()));
        println!("filter result: {res:?}");

        assert_eq!(res.kind(), expected);
    }

    /// Sanity test for empty [`RegexSet`] behaviour.
    #[test]
    fn empty_regex_set() {
        let set = FileFilter::make_regex_set(None).unwrap();
        assert!(!set.is_match("/path/to/some/file"));
    }

    /// Return path to the $HOME directory without trailing slash.
    fn clean_home() -> String {
        env::var("HOME").unwrap().trim_end_matches('/').into()
    }

    #[rstest]
    #[case(&format!("{}/.config/gcloud/some_file", clean_home()), DetourKind::Error)]
    #[case("/root/.config/gcloud/some_file", DetourKind::Success)]
    #[case("/root/.nuget/packages/microsoft.azure.amqp", DetourKind::Success)]
    fn not_found_set(#[case] path: &str, #[case] expected: DetourKind) {
        let filter = FileFilter::new(Default::default());
        let res = filter.continue_or_bypass_with(path, false, || Bypass::IgnoredFile("".into()));
        println!("filter result: {res:?}");

        assert_eq!(res.kind(), expected);
    }
}
