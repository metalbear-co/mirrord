use std::{fs::File, io::Read, path::PathBuf};

use crate::error::DocsError;

// TODO(alex): Support specifying a path.
/// Converts all files in the [`glob::glob`] pattern defined within, in the current directory,
/// into a `Vec<String>`.
/// All files are read in parallel to make the best of disk `reads` (assuming SSDs in this case)
/// performance using a threadpool.
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn parse_files(path: PathBuf) -> Result<Vec<syn::File>, DocsError> {
    let paths = glob::glob(&format!("{}/**/*.rs", path.to_string_lossy()))?;

    paths
        .into_iter()
        .filter_map(Result::ok)
        .map(|path| {
            let mut file = File::open(path)?;
            let mut source = String::new();
            file.read_to_string(&mut source)?;

            syn::parse_file(&source).map_err(Into::into)
        })
        .collect()
}
