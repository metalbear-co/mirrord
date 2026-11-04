#[cfg(unix)]
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
use std::{fs, path::Path, process::Command};

use super::{IgnoreRules, IgnoreSnapshot, IgnoreSource};

/// A source rooted at `root` with `lines`.
fn source(root: &str, lines: &[&str]) -> IgnoreSource {
    IgnoreSource {
        root: root.as_bytes().to_vec(),
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
    }
}

fn write(repo: &Path, path: &str, contents: &str) {
    let path = repo.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Whether git ignores `path`, relative to the root of `repo`, by the rules alone.
// Git is an `.exe` on Windows, which `Command::new` finds.
#[allow(clippy::disallowed_methods)]
fn git_ignores(repo: &Path, global_excludes: &Path, path: &str) -> bool {
    let status = Command::new("git")
        .arg("-c")
        .arg(format!("core.excludesFile={}", global_excludes.display()))
        .args(["check-ignore", "--no-index", "--quiet", path])
        .current_dir(repo)
        // Keeps the developer's own configuration out of the comparison.
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();

    match status.code() {
        Some(0) => true,
        Some(1) => false,
        code => panic!("`git check-ignore {path}` failed with {code:?}"),
    }
}

/// The sidecar keeps on the volume exactly the paths that the CLI leaves out of a push, so both
/// must ignore what git ignores. An entry is often a service's directory in a monorepo, where the
/// repository's root `.gitignore`, `.git/info/exclude` and the global excludes file apply with
/// patterns relative to the repository's root, and nested `.gitignore` files, negations and
/// ignored directories interact as git defines.
#[test]
// Git is an `.exe` on Windows, which `Command::new` finds.
#[allow(clippy::disallowed_methods)]
fn rules_ignore_what_git_ignores() {
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(status.success());

    let global = ["*.swp"];
    let exclude = ["*.secret"];
    let root = [
        "node_modules/",
        "/services/web/dist",
        "services/*/generated/",
        "*.log",
        "!keep.log",
    ];
    let web = ["tmp/", "!tmp/keep.txt", "/cache"];
    let src = ["*.gen.js", "!important.gen.js"];

    let global_excludes = repo.join("global-excludes");
    fs::write(&global_excludes, global.join("\n")).unwrap();
    write(repo, ".git/info/exclude", &exclude.join("\n"));
    write(repo, ".gitignore", &root.join("\n"));
    write(repo, "services/web/.gitignore", &web.join("\n"));
    write(repo, "services/web/src/.gitignore", &src.join("\n"));

    let paths = [
        "app.js",
        "debug.log",
        "keep.log",
        "token.secret",
        ".app.js.swp",
        "node_modules/pkg/index.js",
        "src/node_modules/pkg/index.js",
        "dist/bundle.js",
        "generated/client.js",
        "tmp/keep.txt",
        "tmp/other.txt",
        "cache/entry",
        "src/cache/entry",
        "src/app.js",
        "src/client.gen.js",
        "src/important.gen.js",
        "src/nested/client.gen.js",
    ];
    for path in paths {
        write(repo, &format!("services/web/{path}"), "");
    }

    let rules = IgnoreRules::new(IgnoreSnapshot {
        prefix: b"services/web".to_vec(),
        sources: vec![
            source("", &global),
            source("", &exclude),
            source("", &root),
            source("services/web", &web),
            source("services/web/src", &src),
        ],
    });

    for path in paths {
        assert_eq!(
            rules.is_ignored(Path::new(path), false),
            git_ignores(repo, &global_excludes, &format!("services/web/{path}")),
            "{path}"
        );
    }

    for directory in ["dist", "tmp", "node_modules", "src", "cache", "src/cache"] {
        assert_eq!(
            rules.is_ignored(Path::new(directory), true),
            git_ignores(repo, &global_excludes, &format!("services/web/{directory}")),
            "{directory}/"
        );
    }

    // A walk decides on the children of a directory that it kept, without the directories above.
    for path in paths {
        let path = Path::new(path);
        let parent = path.parent().unwrap();
        if rules.is_ignored(parent, true) {
            continue;
        }

        assert_eq!(
            rules
                .directory(parent)
                .is_ignored(path.file_name().unwrap(), false),
            rules.is_ignored(path, false),
            "{}",
            path.display()
        );
    }
}

/// The last source decides, so that it can both ignore more and re-include what an earlier source
/// ignores. The directories of version control systems are never pushed, whatever the rules say,
/// and a path that leaves the entry is never taken for one of its paths.
#[test]
fn the_last_source_has_the_last_word_except_on_vcs_directories() {
    let rules = IgnoreRules::new(IgnoreSnapshot {
        prefix: b"web".to_vec(),
        sources: vec![
            source("", &["*.log", "dist/"]),
            source("web", &["coverage/", "!debug.log", "!.git", "!.jj"]),
        ],
    });

    assert!(rules.is_ignored(Path::new("coverage/index.html"), false));
    assert!(rules.is_ignored(Path::new("error.log"), false));
    assert!(!rules.is_ignored(Path::new("debug.log"), false));
    assert!(rules.is_ignored(Path::new("dist/debug.log"), false));
    assert!(rules.is_ignored(Path::new(".git/HEAD"), false));
    assert!(rules.is_ignored(Path::new(".jj"), true));
    assert!(rules.is_ignored(Path::new("../outside.js"), false));
    assert!(!rules.is_ignored(Path::new("app.js"), false));
}

/// A `.gitignore` applies below its directory even when the directory's name isn't UTF-8, so the
/// CLI doesn't push a file that such a `.gitignore` excludes.
#[cfg(unix)]
#[test]
fn a_gitignore_in_a_directory_whose_name_isnt_utf8_applies() {
    let directory = Path::new(OsStr::from_bytes(b"caf\xe9"));
    let mut rules = IgnoreRules::new(IgnoreSnapshot::default());
    rules.add_gitignore(directory, vec!["private-key.pem".to_owned()]);

    assert!(rules.is_ignored(&directory.join("private-key.pem"), false));
    assert!(!rules.is_ignored(Path::new("private-key.pem"), false));
}
