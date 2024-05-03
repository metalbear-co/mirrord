use std::{fs::File, io::Read, path::PathBuf, sync::mpsc::channel};

use threadpool::ThreadPool;

use crate::error::DocsError;
/// Glues all the `Vec<String>` docs into one big `String`.
///
/// It can also be used to filter out docs with meta comments, such as `${internal}`.
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn pretty_docs(mut docs: Vec<String>) -> String {
    for doc in docs.iter_mut() {
        // removes docs that we don't want in `configuration.md`
        if doc.contains(r"<!--${internal}-->") {
            return "".to_string();
        }

        // `trim` is too aggressive, we just want to remove 1 whitespace
        if doc.starts_with(' ') {
            doc.remove(0);
        }
    }
    [docs.concat(), "\n".to_string()].concat()
}

// TODO(alex): Support specifying a path.
/// Converts all files in the [`glob::glob`] pattern defined within, in the current directory,
/// into a `Vec<String>`.
/// All files are read in parallel to make the best of disk `reads` (assuming SSDs in this case)
/// performance using a threadpool.
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn files_to_string(path: PathBuf) -> Result<Vec<String>, DocsError> {
    let paths = glob::glob(&format!("{}/**/*.rs", path.to_string_lossy()))?;

    let pool = ThreadPool::new(4);

    let file_processor = |path: PathBuf| {
        let mut file = File::open(&path)?;
        let mut source = String::with_capacity(30 * 1024);
        let read_amount = file.read_to_string(&mut source)?;
        let file_read = source
            .get(..read_amount)
            .ok_or(DocsError::ReadOutOfBounds)?;

        Ok::<_, DocsError>(String::from(file_read))
    };

    let (tx, rx) = channel();

    paths.for_each(|path| {
        let tx = tx.clone();
        pool.execute(move || {
            let file = file_processor(path.unwrap());
            tx.send(file).unwrap();
        });
    });

    drop(tx);

    let mut files = Vec::new();
    while let Ok(Ok(result)) = rx.recv() {
        files.push(result);
    }

    Ok(files)
}

/// Parses the `files` into a collection of [`syn::File`].
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn parse_string_files(files: Vec<String>) -> Vec<syn::File> {
    files
        .into_iter()
        .map(|raw_contents| syn::parse_file(&raw_contents))
        .filter_map(Result::ok)
        .collect::<Vec<_>>()
}
