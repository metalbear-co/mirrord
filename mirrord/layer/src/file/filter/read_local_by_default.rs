use std::env;

use regex::RegexSetBuilder;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
pub fn regex_set_builder() -> RegexSetBuilder {
    RegexSetBuilder::new([
        r"^.+\.so$",
        r"^.+\.d$",
        r"^.+\.pyc$",
        r"^.+\.py$",
        r"^.+\.jar$",
        r"^.+\.js$",
        r"^.+\.pth$",
        r"^.+\.plist$",
        r"^.*venv\.cfg$",
        r"^/proc(/|$)",
        r"^/sys(/|$)",
        r"^/lib(/|$)",
        r"^/etc(/|$)",
        r"^/usr(/|$).*$",
        r"^/home(/|$).*$",
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
        // Any path under the standard temp directory.
        &format!("^{}", env::temp_dir().to_string_lossy()),
        // Any path with the current dir's whole path as a substring of its path.
        &format!("^.*{}.*$", env::current_dir().unwrap().to_string_lossy()),
        // Any path with the current exe's whole path as a substring of its path.
        &format!("^.*{}.*$", env::current_exe().unwrap().to_string_lossy()),
        "^/$", // root
    ])
}
