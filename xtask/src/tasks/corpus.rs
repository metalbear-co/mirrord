//! Vendors the `metalbear-co/docs` and `metalbear-co/skills` repos into `mirrord/mcp/corpus`, the
//! content `mirrord mcp` answers from without network access.
//!
//! Each corpus is pinned to an upstream commit in `<name>.pin.json`, and its files are kept
//! byte-identical to that commit. The release PR bumps the pins (`sync --bump`), and CI checks that
//! the vendored files still match their pins (`check`), so a release is reproducible from its
//! commit.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// An upstream repo vendored into the corpus.
struct Corpus {
    /// The directory under `mirrord/mcp/corpus` it is vendored into, and the name of its pin.
    name: &'static str,
    repo: &'static str,
    /// The directory of the repo holding the content, vendored at the same paths relative to it.
    root: &'static str,
    /// The extensions of the files under `root` that are vendored. `mirrord mcp` serves text only,
    /// so images and other binary files upstream are skipped rather than failing the sync.
    extensions: &'static [&'static str],
    /// Files under `root` that aren't vendored.
    exclude: &'static [&'static str],
}

const CORPORA: &[Corpus] = &[
    Corpus {
        name: "docs",
        repo: "metalbear-co/docs",
        root: "docs",
        extensions: &["md"],
        // GitBook's navigation, not a page.
        exclude: &["SUMMARY.md"],
    },
    Corpus {
        name: "skills",
        repo: "metalbear-co/skills",
        root: "skills",
        // Skills point the agent at the files bundled with them, such as Helm values, so those are
        // served too.
        extensions: &["md", "json", "yaml", "yml"],
        // Copies of the config schema and of the config reference generated from it, which lag
        // behind the schema compiled into `mirrord mcp` and served by its config tools.
        exclude: &[
            "mirrord-config/references/configuration.md",
            "mirrord-config/references/schema.json",
            "mirrord-ci/references/schema.json",
            "mirrord-db-branching/references/db-branches-schema.json",
        ],
    },
];

/// The upstream commit a corpus is vendored from, stored next to it as `<name>.pin.json`.
#[derive(Serialize, Deserialize)]
struct Pin {
    repo: String,
    commit: String,
    /// When the pin was last bumped, in RFC 3339.
    synced_at: String,
}

/// Vendors every corpus from its pinned commit, replacing what is vendored. With `bump`, first
/// moves the pins to upstream HEAD.
pub fn sync(bump: bool) -> Result<()> {
    for corpus in CORPORA {
        let pin_path = corpus_dir().join(format!("{}.pin.json", corpus.name));
        let pin = if bump {
            Pin {
                repo: corpus.repo.to_owned(),
                commit: upstream_head(corpus.repo)?,
                synced_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            }
        } else {
            read_pin(&pin_path)?
        };

        let files = fetch(corpus, &pin)?;
        // Written once its commit is known to sync, so a failed sync keeps the previous pin.
        if bump {
            fs::write(&pin_path, serde_json::to_string_pretty(&pin)? + "\n")
                .with_context(|| format!("failed to write {}", pin_path.display()))?;
        }
        let dir = corpus_dir().join(corpus.name);
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
        }
        for (path, contents) in &files {
            let path = dir.join(path);
            fs::create_dir_all(
                path.parent()
                    .expect("vendored files are under the corpus dir"),
            )?;
            fs::write(&path, contents)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }

        println!(
            "✓ {}: {} files from {}@{}",
            corpus.name,
            files.len(),
            pin.repo,
            pin.commit
        );
    }

    Ok(())
}

/// Fails unless every corpus holds exactly the files of its pinned commit, byte for byte.
pub fn check() -> Result<()> {
    let mut mismatches = Vec::new();
    for corpus in CORPORA {
        let pin = read_pin(&corpus_dir().join(format!("{}.pin.json", corpus.name)))?;
        ensure!(
            pin.repo == corpus.repo,
            "{}.pin.json pins {}, expected {}",
            corpus.name,
            pin.repo,
            corpus.repo
        );

        let upstream = fetch(corpus, &pin)?;
        let dir = corpus_dir().join(corpus.name);
        let vendored = read_tree(&dir, &dir)?;

        for (path, contents) in &upstream {
            match vendored.get(path) {
                None => mismatches.push(format!("{}/{path}: missing", corpus.name)),
                Some(vendored) if vendored != contents => {
                    mismatches.push(format!("{}/{path}: differs from upstream", corpus.name))
                }
                Some(_) => {}
            }
        }
        for path in vendored.keys().filter(|path| !upstream.contains_key(*path)) {
            mismatches.push(format!("{}/{path}: not in upstream", corpus.name));
        }
    }

    if !mismatches.is_empty() {
        bail!(
            "the vendored corpus doesn't match its pins, run `cargo xtask corpus sync`:\n{}",
            mismatches.join("\n")
        );
    }

    println!("✓ corpus matches its pins");
    Ok(())
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../mirrord/mcp/corpus")
}

fn read_pin(path: &Path) -> Result<Pin> {
    let pin =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&pin).with_context(|| format!("invalid pin {}", path.display()))
}

fn upstream_head(repo: &str) -> Result<String> {
    let output = git(None, &["ls-remote", &repo_url(repo), "HEAD"])?;
    let output = String::from_utf8(output)?;
    output
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .with_context(|| format!("no HEAD in {repo}"))
}

/// The files of `corpus` at its pinned commit, keyed by their path relative to its root. Contents
/// are the raw blobs, so no checkout filter or line-ending conversion touches them.
fn fetch(corpus: &Corpus, pin: &Pin) -> Result<BTreeMap<String, Vec<u8>>> {
    let checkout = env::temp_dir().join(format!("mirrord-xtask-corpus-{}", corpus.name));
    if checkout.exists() {
        fs::remove_dir_all(&checkout)?;
    }
    fs::create_dir_all(&checkout)?;

    git(Some(&checkout), &["init", "--quiet"])?;
    git(
        Some(&checkout),
        &[
            "fetch",
            "--quiet",
            "--depth",
            "1",
            &repo_url(&pin.repo),
            &pin.commit,
        ],
    )
    .with_context(|| format!("failed to fetch {}@{}", pin.repo, pin.commit))?;

    let listing = git(
        Some(&checkout),
        &[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "FETCH_HEAD",
            "--",
            corpus.root,
        ],
    )?;

    let mut files = BTreeMap::new();
    for path in listing
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)?;
        let relative = path
            .strip_prefix(corpus.root)
            .and_then(|path| path.strip_prefix('/'))
            .context("git listed a file outside the corpus root")?;
        let hidden = relative.split('/').any(|segment| segment.starts_with('.'));
        let vendored = relative
            .rsplit_once('.')
            .is_some_and(|(_, extension)| corpus.extensions.contains(&extension));
        if hidden || !vendored || corpus.exclude.contains(&relative) {
            continue;
        }

        let contents = git(
            Some(&checkout),
            &["cat-file", "blob", &format!("FETCH_HEAD:{path}")],
        )?;
        ensure!(
            std::str::from_utf8(&contents).is_ok(),
            "{path} in {}@{} isn't UTF-8 text, which `mirrord mcp` can't serve",
            pin.repo,
            pin.commit
        );
        files.insert(relative.to_owned(), contents);
    }

    fs::remove_dir_all(&checkout)?;
    ensure!(
        !files.is_empty(),
        "{}@{} has nothing to vendor under `{}`",
        pin.repo,
        pin.commit,
        corpus.root
    );
    Ok(files)
}

/// Every file under `dir`, keyed by its `/`-separated path relative to `root`.
fn read_tree(root: &Path, dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    if !dir.exists() {
        return Ok(files);
    }

    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(read_tree(root, &path)?);
        } else {
            let relative = path
                .strip_prefix(root)?
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.insert(relative, fs::read(&path)?);
        }
    }
    Ok(files)
}

fn repo_url(repo: &str) -> String {
    format!("https://github.com/{repo}")
}

/// Runs git, in `dir` if given, and returns its stdout.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command.args(args).output().context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
