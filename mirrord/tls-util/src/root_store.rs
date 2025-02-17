use std::{
    fs::{self, File},
    io::BufReader,
    path::PathBuf,
};

use rustls::RootCertStore;

/// Builds a [`RootCertStore`] from all certificates founds under the given paths.
///
/// Accepts paths to:
/// 1. PEM files
/// 2. Directories containing PEM files
///
/// Directories are not traversed recursively.
///
/// Does not fail, see [`RootCertStore::add_parsable_certificates`] for rationale.
/// Instead, it logs warnings and errors.
pub fn best_effort_root_store<P: IntoIterator<Item = PathBuf>>(paths: P) -> RootCertStore {
    let mut root_store = RootCertStore::empty();

    let mut queue = paths
        .into_iter()
        .zip(std::iter::repeat(true))
        .collect::<Vec<_>>();

    while let Some((path, read_if_dir)) = queue.pop() {
        let is_dir = path.is_dir();

        if is_dir && read_if_dir {
            let Ok(entries) = fs::read_dir(&path).inspect_err(|error| {
                tracing::error!(
                    %error,
                    ?path,
                    "Failed to list a directory when building a root cert store."
                );
            }) else {
                continue;
            };

            entries
                .filter_map(|result| {
                    result
                        .inspect_err(|error| {
                            tracing::error!(
                                %error,
                                ?path,
                                "Failed to list a directory when building a root cert store."
                            )
                        })
                        .ok()
                })
                .for_each(|entry| queue.push((entry.path(), false)))
        } else if is_dir {
            continue;
        } else {
            let Ok(file) = File::open(&path).inspect_err(|error| {
                tracing::error!(
                    %error,
                    ?path,
                    "Failed to open a file when building a root cert store."
                );
            }) else {
                continue;
            };

            let mut file = BufReader::new(file);
            let certs = rustls_pemfile::certs(&mut file).filter_map(|result| {
                result
                    .inspect_err(|error| {
                        tracing::error!(
                            %error,
                            ?path,
                            "Failed to parse a file when building a root cert store.",
                        )
                    })
                    .ok()
            });

            let (added, ignored) = root_store.add_parsable_certificates(certs);

            if ignored > 0 {
                tracing::warn!(
                    ?path,
                    added,
                    "Ignored {ignored} invalid certificate(s) when building a root cert store."
                );
            }
        }
    }

    root_store
}
