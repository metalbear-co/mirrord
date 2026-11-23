//! Packs the docs and skills that `mirrord mcp` answers from into a gzipped tar that
//! `src/corpus.rs` embeds, so the files take less space in the binary.
//!
//! They are read from `docs/docs` and `skills/skills` at the repository root, so the binary serves
//! the docs and skills of the checkout it is built from. The files are read from disk, untracked
//! ones included, since a Docker build from a Git context has no `.git` to list them with. Only the
//! text files that `mirrord mcp` serves are packed.

use std::{
    env,
    ffi::OsStr,
    fs::{self, File},
    io,
    ops::Not,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::GzEncoder};

/// A directory of the repository whose files are packed.
struct Corpus {
    /// The directory the files are packed under, which `src/corpus.rs` keys them by, e.g.
    /// `skills/mirrord-up/SKILL.md`.
    name: &'static str,
    /// The directory, relative to the repository root, whose files are packed under `name` at their
    /// paths relative to this directory.
    root: &'static str,
    /// The extensions of the files that are packed. `mirrord mcp` serves text only, so images and
    /// other binary files are left out.
    extensions: &'static [&'static str],
    /// Files under `root`, as `/`-separated paths relative to it, that aren't packed.
    exclude: &'static [&'static str],
}

const CORPORA: [Corpus; 2] = [
    Corpus {
        name: "docs",
        root: "docs/docs",
        extensions: &["md"],
        // GitBook's navigation, not a page.
        exclude: &["SUMMARY.md"],
    },
    Corpus {
        name: "skills",
        root: "skills/skills",
        // Skills point the agent at the files bundled with them, such as Helm values, so those are
        // served too.
        extensions: &["md", "json", "yaml", "yml"],
        // Copies of the config schemas and of the config reference generated from them, which lag
        // behind the schemas compiled into `mirrord mcp` and served by its config tools.
        exclude: &[
            "mirrord-config/references/configuration.md",
            "mirrord-config/references/schema.json",
            "mirrord-ci/references/schema.json",
            "mirrord-db-branching/references/db-branches-schema.json",
            "mirrord-up/references/schema.json",
        ],
    },
];

fn main() -> io::Result<()> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    // The path in the archive, and the file it is read from.
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    for corpus in &CORPORA {
        let root = repo.join(corpus.root);
        println!("cargo:rerun-if-changed={}", root.display());
        let before = files.len();
        collect(corpus, &root, &root, &mut files)?;
        if files.len() == before {
            return Err(io::Error::other(format!(
                "nothing to pack under {}",
                root.display()
            )));
        }
    }
    // Sorted, with fixed metadata below, so the same content always packs to the same bytes.
    files.sort();

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let encoder = GzEncoder::new(
        File::create(out.join("corpus.tar.gz"))?,
        Compression::best(),
    );
    let mut archive = tar::Builder::new(encoder);
    for (path, source) in files {
        let contents = String::from_utf8(fs::read(&source)?).map_err(|_| {
            io::Error::other(format!(
                "{} isn't UTF-8 text, which `mirrord mcp` can't serve",
                source.display()
            ))
        })?;
        // Packed as read: `src/corpus.rs` parses front matter with LF only, and the root
        // `.gitattributes` keeps these trees LF in every checkout, Windows included.
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        archive.append_data(&mut header, &path, contents.as_bytes())?;
    }
    archive.into_inner()?.finish()?;

    Ok(())
}

/// Adds every file under `dir` that `corpus` packs to `files`. Hidden files and directories, such
/// as GitBook's `.gitbook/`, are skipped.
fn collect(
    corpus: &Corpus,
    root: &Path,
    dir: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> io::Result<()> {
    let entries = fs::read_dir(dir).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to read {}: {error}", dir.display()),
        )
    })?;
    for entry in entries {
        let path = entry?.path();
        let hidden = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() {
            collect(corpus, root, &path, files)?;
            continue;
        }

        let packed = path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| corpus.extensions.contains(&extension));
        let relative = path.strip_prefix(root).expect("walked from root");
        let excluded = corpus
            .exclude
            .iter()
            .any(|excluded| relative == Path::new(excluded));
        if packed && excluded.not() {
            files.push((Path::new(corpus.name).join(relative), path));
        }
    }
    Ok(())
}
