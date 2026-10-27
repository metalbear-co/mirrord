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

    if let Ok(cwd) = env::current_dir() {
        patterns.extend(cwd_patterns(&cwd));
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

/// The patterns that keep the working directory local: everything under it, and each of its
/// ancestors by exact match.
///
/// Resolving a path under the working directory walks its ancestors first: Node's
/// `realpathSync` calls `lstat` on `/workspace`, then `/workspace/app`, and so on. If an ancestor
/// went to the remote, a target without that directory fails the whole walk with `ENOENT`. The
/// exact match keeps only the directory itself local, so its other entries still follow the
/// usual rules.
///
/// Root gives no patterns: a lot of containers use it as `WORKDIR`, and making *everything*
/// local is confusing. It's also local by default already.
pub(crate) fn cwd_patterns(cwd: &Path) -> Vec<String> {
    if cwd.as_os_str().as_encoded_bytes() == b"/" {
        return Vec::new();
    }

    // `current_dir` may or may not end in '/', so trim it and add our own.
    let escape = |dir: &Path| regex::escape(dir.to_string_lossy().trim_end_matches('/'));

    let mut patterns = vec![format!("^{}(/|$)", escape(cwd))];
    patterns.extend(
        cwd.ancestors()
            .skip(1)
            .filter(|ancestor| ancestor.parent().is_some())
            .map(|ancestor| format!("^{}/?$", escape(ancestor))),
    );
    patterns
}
