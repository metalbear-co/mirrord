//! Finds the user's projects on disk, so picking what to run is choosing from a list rather than
//! starting the TUI in the right folder or typing paths.
//!
//! A project is a folder holding a file that a build tool or a container reads
//! ([`MARKERS`]). The search starts at the home folder and skips what never holds the user's own
//! code (dependencies, build output, hidden folders), and it stops at fixed limits so a huge home
//! folder costs a few seconds in the background at most, never a frozen screen.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Files that make a folder a project, in the order their name is shown as the folder's kind.
pub const MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "Dockerfile",
];

/// Folders that hold dependencies, build output or the system's own files, never projects worth
/// running. Hidden folders are skipped as well.
const SKIPPED: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "out",
    "venv",
    "env",
    "__pycache__",
    "site-packages",
    "Library",
    "Applications",
    "Movies",
    "Music",
    "Pictures",
];

/// How deep under the home folder the search goes: `~/code/shop/services/checkout` is 4.
const MAX_DEPTH: usize = 5;

/// Most folders read before the search stops, so a home folder with millions of entries does not
/// keep a thread busy.
const MAX_VISITED: usize = 30_000;

/// Most projects listed. More than this is not a list anyone scrolls; typing narrows it.
const MAX_PROJECTS: usize = 2_000;

/// Longest the search runs.
const TIME_BUDGET: Duration = Duration::from_secs(4);

/// The marker file that makes `dir` a project, if it is one.
pub fn marker(dir: &Path) -> Option<&'static str> {
    MARKERS
        .iter()
        .copied()
        .find(|marker| dir.join(marker).is_file())
}

/// Every project under `root`, closest first, nested ones included, since a repository often
/// holds several services. Blocks: run it off the async runtime.
pub fn scan(root: &Path) -> Vec<PathBuf> {
    let started = Instant::now();
    let mut projects = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0)]);
    let mut visited = 0;

    while let Some((dir, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_VISITED
            || projects.len() >= MAX_PROJECTS
            || started.elapsed() > TIME_BUDGET
        {
            break;
        }
        if depth > 0 && marker(&dir).is_some() {
            projects.push(dir.clone());
        }
        if depth == MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && !SKIPPED.contains(&name.as_ref())
            })
            .map(|entry| entry.path())
            .collect();
        children.sort();
        queue.extend(children.into_iter().map(|child| (child, depth + 1)));
    }
    projects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_projects_and_skips_dependencies_and_hidden_folders() {
        let root = std::env::temp_dir().join(format!("mirrord-tui-scan-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&root);
        for (dir, file) in [
            ("shop", "package.json"),
            ("shop/services/checkout", "go.mod"),
            ("shop/node_modules/left-pad", "package.json"),
            (".cache/thing", "Cargo.toml"),
            ("notes", "todo.txt"),
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join(file), "").unwrap();
        }

        let found = scan(&root);
        _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            found,
            vec![root.join("shop"), root.join("shop/services/checkout")]
        );
    }
}
