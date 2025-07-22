use std::env;

use regex::RegexSetBuilder;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
pub fn regex_set_builder() -> RegexSetBuilder {
    let mut patterns: Vec<String> = [
        r".\.so$",
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
        r"^/snap(/|$)",
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
        patterns.push(format!(
            "^{}(/|$)",
            // We trim any trailing '/' and add it above, so that the regex is correct whether
            // current_dir returns a trailing '/' or not.
            regex::escape(cwd.to_string_lossy().trim_end_matches('/'))
        ));
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
