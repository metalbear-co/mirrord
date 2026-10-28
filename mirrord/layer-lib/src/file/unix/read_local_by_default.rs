use std::{env, path::Path};

use regex::RegexSetBuilder;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
pub fn regex_set_builder() -> RegexSetBuilder {
    let mut patterns: Vec<String> = [
        r".\.so$",
        r".\.sh$",
        r".\.rb$",
        r".\.d$",
        r".\.pyc$",
        r".\.py$",
        r".\.jar$",
        r".\.class$",
        r".\.js$",
        r".\.pth$",
        r".\.plist$",
        r"venv\.cfg$",
        r"^/proc(/|$)",
        r"^/sys(/|$)",
        r"^/lib(/|$)",
        r"^/etc(/|$)",
        r"^/usr(/|$)",
        r"^/home(/|$)",
        r"^/bin(/|$)",
        r"^/sbin(/|$)",
        r"^/dev(/|$)",
        r"^/opt(/|$)",
        r"^/tmp(/|$)",
        r"^/var/tmp(/|$)",
        r"^/var/db/timezone/zoneinfo(/|$)",
        r"^/snap(/|$)",
        r"^/run(/|$)",
        r"^/selinux(/|$)",
        // support for nixOS.
        r"^/nix(/|$)",
        r".+\.asdf/.+",
        r"^/home/iojs(/|$)",
        r"^/home/runner(/|$)",
        // dotnet: `/tmp/clr-debug-pipe-1`
        r"clr-.*-pipe-",
        // dotnet: `/home/{username}/{project}.pdb`
        r"\.pdb$",
        // dotnet: `/home/{username}/{project}.dll`
        r"\.dll$",
        // jvm.cfg or ANYTHING/jvm.cfg
        r".*(^|/)jvm\.cfg$",
        // TODO: `node` searches for this file in multiple directories, bypassing some of our
        // ignore regexes, maybe other "project runners" will do the same.
        r"/package\.json$",
        r"/\.yarnrc$",
        r"/\.yarnrc.yml$",
        r"/\.yarnrc.json$",
        r"/node_modules",
        // asdf
        r".*/\.tool-versions$",
        // WSL quirks
        "^/Program Files",
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
        #[cfg(target_os = "macos")]
        r"^/Network",
        // Any path under the standard temp directory.
        &format!("^{}", regex::escape(&env::temp_dir().to_string_lossy())),
        "^/$", // root
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if let Some(cwd) = env::current_dir().ok().and_then(|cwd| cwd_pattern(&cwd)) {
        patterns.push(cwd);
    }
    if let Ok(executable) = env::current_exe() {
        patterns.push(format!("{}$", regex::escape(&executable.to_string_lossy())));
    }

    // Hidden files and directories in $HOME.
    if let Ok(home) = env::var("HOME") {
        patterns.push(format!(
            "^{}/\\.",
            regex::escape(home.trim_end_matches('/'))
        ))
    }

    RegexSetBuilder::new(patterns)
}

/// Path patterns whose entry is described locally by default, while opening them, or anything
/// under them, still follows the usual rules. These are the working directory's ancestors.
///
/// Resolving a path under the working directory walks its ancestors first: Node's
/// `realpathSync` calls `lstat` on `/workspace`, then `/workspace/app`, and so on. If an ancestor
/// went to the remote, a target without that directory fails the whole walk with `ENOENT`.
///
/// Only calls that describe the entry (`stat`, `access`, `readlink`, `realpath`, `statfs`) use
/// these. Opening an ancestor stays with the usual rules on purpose: an `openat` relative to a
/// local directory fd goes to the local directory, because the layer can only follow remote fds.
/// So a local fd for `/workspace` would send `openat(fd, "remote-only.txt")` to the local disk,
/// while `open("/workspace/remote-only.txt")` reaches the target.
pub fn entry_regex_set_builder() -> RegexSetBuilder {
    let patterns = env::current_dir()
        .map(|cwd| cwd_ancestor_patterns(&cwd))
        .unwrap_or_default();

    RegexSetBuilder::new(patterns)
}

/// The pattern that keeps the working directory and everything under it local.
///
/// Root gives no pattern: a lot of containers use it as `WORKDIR`, and making *everything*
/// local is confusing. It's also local by default already.
pub(crate) fn cwd_pattern(cwd: &Path) -> Option<String> {
    (cwd.as_os_str().as_encoded_bytes() != b"/").then(|| format!("^{}(/|$)", trimmed(cwd)))
}

/// One exact-match pattern per ancestor of the working directory, so only the directory itself
/// matches, and its other entries still follow the usual rules.
///
/// Root is never among them: it's the one ancestor without a parent. Even if it got through, it
/// would only repeat the `^/$` root entry in [`regex_set_builder`], since `/` trims to `^/?$`.
/// The root that must stay out is root as the working directory, whose `^/(/|$)` would make
/// every path local, and [`cwd_pattern`] skips that.
pub(crate) fn cwd_ancestor_patterns(cwd: &Path) -> Vec<String> {
    cwd.ancestors()
        .skip(1)
        .filter(|ancestor| ancestor.parent().is_some())
        .map(|ancestor| format!("^{}/?$", trimmed(ancestor)))
        .collect()
}

/// `dir` escaped for a regex, without a trailing '/', so a pattern can add its own whether
/// `current_dir` ends in one or not.
fn trimmed(dir: &Path) -> String {
    regex::escape(dir.to_string_lossy().trim_end_matches('/'))
}
