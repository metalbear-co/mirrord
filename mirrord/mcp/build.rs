//! Packs the vendored docs and skills in `corpus/` into a gzipped tar that `src/corpus.rs` embeds,
//! so the files take less space in the binary.

use std::{
    env,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::GzEncoder};

/// The directories of `corpus/` that are embedded. Their pins are embedded by `src/corpus.rs`.
const CORPORA: [&str; 2] = ["docs", "skills"];

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=corpus");

    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let mut files = Vec::new();
    for name in CORPORA {
        collect(&corpus, &corpus.join(name), &mut files)?;
    }
    // Sorted, with fixed metadata below, so the same corpus always packs to the same bytes.
    files.sort();

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let encoder = GzEncoder::new(
        File::create(out.join("corpus.tar.gz"))?,
        Compression::best(),
    );
    let mut archive = tar::Builder::new(encoder);
    for path in files {
        let contents = fs::read(corpus.join(&path))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        archive.append_data(&mut header, &path, contents.as_slice())?;
    }
    archive.into_inner()?.finish()?;

    Ok(())
}

/// Adds the path of every file under `dir`, relative to `root`, to `files`.
fn collect(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else {
            files.push(
                path.strip_prefix(root)
                    .expect("walked from root")
                    .to_owned(),
            );
        }
    }
    Ok(())
}
