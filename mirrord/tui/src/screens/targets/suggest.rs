//! Discovers plausible local run commands for the working directory, and
//! path helpers for typing commands comfortably.
//!
//! The Command field offers the detected commands via Tab while editing.
//! They are guesses about how the project in the TUI's working directory
//! starts, nothing more - the user always can (and often must) type their
//! own.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Detected run commands for a service's directory (the working directory
/// when unset), best guess first. Rediscovered per call so edits to the
/// Directory field immediately change what the Command field offers.
pub fn commands_in(dir: Option<&str>) -> Vec<String> {
    discover(Path::new(dir.unwrap_or(".")))
}

/// Looks for project markers in `dir` and maps each to the conventional
/// dev command. Pure over the directory, so tests can point it at
/// fixtures.
fn discover(dir: &Path) -> Vec<String> {
    let mut commands = Vec::new();

    // package.json scripts: `dev` is what dev servers conventionally hide
    // behind; `start` is the npm default.
    if let Ok(package) = std::fs::read_to_string(dir.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&package)
        && let Some(scripts) = json.get("scripts").and_then(|value| value.as_object())
    {
        if scripts.contains_key("dev") {
            commands.push("npm run dev".to_owned());
        }
        if scripts.contains_key("start") {
            commands.push("npm start".to_owned());
        }
    }

    if dir.join("Cargo.toml").is_file() {
        commands.push("cargo run".to_owned());
    }
    if dir.join("go.mod").is_file() {
        commands.push("go run .".to_owned());
    }
    if dir.join("manage.py").is_file() {
        commands.push("python3 manage.py runserver".to_owned());
    }
    if dir.join("app.py").is_file() {
        commands.push("python3 app.py".to_owned());
    }
    if dir.join("main.py").is_file() {
        commands.push("python3 main.py".to_owned());
    }
    if dir.join("gradlew").is_file() {
        commands.push("./gradlew bootRun".to_owned());
    } else if dir.join("pom.xml").is_file() {
        commands.push("mvn spring-boot:run".to_owned());
    }
    if dir.join("Makefile").is_file() {
        commands.push("make run".to_owned());
    }

    // Executable files in the directory run as `./name` - the natural
    // command for a prebuilt binary like a local test server. Project
    // markers above rank first; these are the fallback.
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut binaries: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let executable = meta.is_file() && !name.starts_with('.') && runnable(&meta, &name);
                executable.then(|| format!("./{name}"))
            })
            .collect();
        binaries.sort();
        commands.extend(binaries);
    }

    // A lone main.go without go.mod ranks last: when a built binary sits
    // next to it, running the binary beats compiling under mirrord.
    if dir.join("main.go").is_file() && !dir.join("go.mod").is_file() {
        commands.push("go run main.go".to_owned());
    }

    commands
}

/// Whether a file in the directory is something a shell would run.
///
/// Unix answers with the executable bit. Windows has no equivalent, so the question becomes whether
/// the name carries an extension the shell knows how to execute.
#[cfg(unix)]
fn runnable(meta: &std::fs::Metadata, _name: &str) -> bool {
    meta.permissions().mode() & 0o111 != 0
}

/// Counterpart of [`runnable`] for platforms without an executable bit.
#[cfg(not(unix))]
fn runnable(_meta: &std::fs::Metadata, name: &str) -> bool {
    const RUNNABLE: [&str; 4] = ["exe", "bat", "cmd", "com"];

    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            RUNNABLE
                .iter()
                .any(|runnable| extension.eq_ignore_ascii_case(runnable))
        })
}

/// Expands a leading `~` to the home directory, so typed paths stay short
/// but the stored command works without a shell.
pub fn expand_home(part: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return part.to_owned();
    };
    let home = home.to_string_lossy();
    match part.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None if part == "~" => home.into_owned(),
        None => part.to_owned(),
    }
}

/// Inverse of [`expand_home`] for display: long absolute paths under the
/// home directory read as `~/…`.
pub fn compress_home(part: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return part.to_owned();
    };
    let home = home.to_string_lossy();
    match part.strip_prefix(home.as_ref()) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => part.to_owned(),
    }
}

/// Completion candidates for a filesystem path prefix: the entries of the
/// prefix's directory whose names start with its last component, sorted,
/// with `/` appended to directories. Candidates keep the `~` form when the
/// prefix used it.
pub fn complete_path(prefix: &str) -> Vec<String> {
    let tilde = prefix == "~" || prefix.starts_with("~/");
    let expanded = expand_home(prefix);

    let (dir, stem) = match expanded.rsplit_once('/') {
        Some(("", stem)) => ("/", stem),
        Some(split) => split,
        None => return Vec::new(),
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(stem) || (stem.is_empty() && name.starts_with('.')) {
                return None;
            }
            let trailer = if entry.file_type().ok()?.is_dir() {
                "/"
            } else {
                ""
            };
            let path = format!("{}/{name}{trailer}", dir.trim_end_matches('/'));
            Some(if tilde { compress_home(&path) } else { path })
        })
        .collect();
    candidates.sort();
    candidates
}

/// The longest prefix shared by every candidate, in characters.
pub fn common_prefix(candidates: &[String]) -> String {
    let Some(first) = candidates.first() else {
        return String::new();
    };
    let mut prefix: &str = first;
    for candidate in &candidates[1..] {
        let shared = prefix
            .char_indices()
            .zip(candidate.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map(|((at, c), _)| at + c.len_utf8())
            .unwrap_or(0);
        prefix = &prefix[..shared];
    }
    prefix.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory seeded with the given files.
    fn fixture(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mirrord-tui-suggest-{}-{:p}",
            std::process::id(),
            files
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    #[test]
    fn discovers_package_json_scripts_before_other_markers() {
        let dir = fixture(&[
            (
                "package.json",
                r#"{"scripts": {"dev": "vite", "start": "node ."}}"#,
            ),
            ("Cargo.toml", ""),
        ]);
        assert_eq!(
            discover(&dir),
            ["npm run dev", "npm start", "cargo run"],
            "package.json scripts rank first"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignores_scripts_mentioned_outside_the_scripts_table() {
        let dir = fixture(&[(
            "package.json",
            r#"{"dependencies": {"dev": "1.0.0", "start": "1.0.0"}}"#,
        )]);
        assert_eq!(
            discover(&dir),
            Vec::<String>::new(),
            "dependency names are not run scripts"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn empty_directory_suggests_nothing() {
        let dir = fixture(&[]);
        assert_eq!(discover(&dir), Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A directory with only a built binary and its source still suggests
    /// runnable commands: the binary as `./name`, the lone main.go via
    /// `go run`.
    /// Unix-only: the fixture makes itself executable with a mode bit, which is the very thing
    /// other platforms do not have.
    #[cfg(unix)]
    #[test]
    fn suggests_executables_and_lone_main_go() {
        let dir = fixture(&[("main.go", "package main"), ("zoo-echo", "")]);
        let binary = dir.join("zoo-echo");
        let mut perms = std::fs::metadata(&binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms).unwrap();

        assert_eq!(discover(&dir), ["./zoo-echo", "go run main.go"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn home_round_trips_through_expand_and_compress() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~/x/y"), format!("{home}/x/y"));
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("/opt/x"), "/opt/x");
        assert_eq!(compress_home(&format!("{home}/x")), "~/x");
        assert_eq!(compress_home("/opt/x"), "/opt/x");
        // `~foo` is a different user's home, not ours - leave it alone.
        assert_eq!(expand_home("~foo/x"), "~foo/x");
    }

    #[test]
    fn completes_paths_marking_directories() {
        let dir = fixture(&[("zoo-echo", ""), ("zoo.yaml", "")]);
        std::fs::create_dir_all(dir.join("zoo-src")).unwrap();

        let prefix = format!("{}/zoo", dir.display());
        assert_eq!(
            complete_path(&prefix),
            [
                format!("{}/zoo-echo", dir.display()),
                format!("{}/zoo-src/", dir.display()),
                format!("{}/zoo.yaml", dir.display()),
            ],
            "all zoo* entries, directories with a trailing slash"
        );
        assert_eq!(
            complete_path(&format!("{}/zoo.", dir.display())),
            [format!("{}/zoo.yaml", dir.display())],
        );
        assert_eq!(
            complete_path(&format!("{}/nope", dir.display())),
            [] as [String; 0]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn common_prefix_spans_all_candidates() {
        let candidates = vec!["zoo-echo".to_owned(), "zoo-src/".to_owned()];
        assert_eq!(common_prefix(&candidates), "zoo-");
        assert_eq!(common_prefix(&[]), "");
        assert_eq!(common_prefix(&["only".to_owned()]), "only");
    }
}
