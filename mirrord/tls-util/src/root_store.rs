use core::fmt;
use std::{
    fs::{self, File},
    io::BufReader,
    path::PathBuf,
};

use rustls::RootCertStore;
use tokio::task::JoinError;
use tracing::Level;

/// Builds a [`RootCertStore`] from all certificates found under the given paths.
///
/// Accepts paths to:
/// 1. PEM files
/// 2. Directories containing PEM files
///
/// Directories are not traversed recursively.
///
/// Fails only when the spawned tokio blocking task panics, see
/// [`RootCertStore::add_parsable_certificates`] for rationale.
/// Logs warnings and errors from processing the certificates.
///
/// See this crate's docs for blocking tasks rationale.
#[tracing::instrument(level = Level::DEBUG, ret)]
pub async fn best_effort_root_store<P: IntoIterator<Item = PathBuf> + fmt::Debug>(
    paths: P,
) -> Result<RootCertStore, JoinError> {
    let mut queue = paths
        .into_iter()
        .zip(std::iter::repeat(true))
        .collect::<Vec<_>>();

    tokio::task::spawn_blocking(move || {
        let mut root_store = RootCertStore::empty();

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
    })
    .await
}

#[cfg(test)]
mod test {
    use std::fs;

    use pem::{EncodeConfig, LineEnding, Pem};

    /// Verifies that [`super::best_effort_root_store`] correctly parses certificates from the given
    /// paths.
    #[tokio::test]
    async fn parse_root_certs() {
        let dir = tempfile::tempdir().unwrap();

        let mut file_idx = 0;
        let mut certs = Vec::new();
        let mut paths = Vec::new();

        // To files in the main directory, each containing one certificate.
        for _ in 0..2 {
            let file = format!("root-{file_idx}.pem");
            file_idx += 1;

            let path = dir.path().join(file);
            let cert = rcgen::generate_simple_self_signed(vec![format!("issuer-{}", certs.len())])
                .unwrap();
            let pem = cert.cert.pem();
            fs::write(&path, &pem).unwrap();
            certs.push(cert);
            paths.push(path);
        }

        // One file in the main directory, containing two certificates.
        let file = format!("root-{file_idx}.pem");
        file_idx += 1;
        let path = dir.path().join(file);
        let cert_1 =
            rcgen::generate_simple_self_signed(vec![format!("issuer-{}", certs.len())]).unwrap();
        let cert_1_der = cert_1.cert.der().to_vec();
        certs.push(cert_1);
        let cert_2 =
            rcgen::generate_simple_self_signed(vec![format!("issuer-{}", certs.len())]).unwrap();
        let cert_2_der = cert_2.cert.der().to_vec();
        certs.push(cert_2);
        let content = pem::encode_many_config(
            &[
                Pem::new("CERTIFICATE", cert_1_der),
                Pem::new("CERTIFICATE", cert_2_der),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        );
        fs::write(&path, &content).unwrap();
        paths.push(path);

        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        paths.push(subdir.clone());

        // One file in a subdirectory, containing one certificate and a malformed entry.
        let file = format!("root-{file_idx}.pem");
        file_idx += 1;
        let path = subdir.join(file);
        let cert =
            rcgen::generate_simple_self_signed(vec![format!("issuer-{}", certs.len())]).unwrap();
        let cert_der = cert.cert.der().to_vec();
        certs.push(cert);
        let content = pem::encode_many_config(
            &[
                Pem::new("CERTIFICATE", cert_der),
                Pem::new("NOT A VALID TAG", b"hello"),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        );
        fs::write(path, content).unwrap();

        // One malformed PEM file in a subdirectory.
        let file = format!("root-{file_idx}.pem");
        file_idx += 1;
        let path = subdir.join(file);
        fs::write(path, b"hello there").unwrap();

        // One file in a subdirectory, containing one certificate and a private key.
        let file = format!("root-{file_idx}.pem");
        let path = subdir.join(file);
        let cert =
            rcgen::generate_simple_self_signed(vec![format!("issuer-{}", certs.len())]).unwrap();
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.key_pair.serialize_der();
        certs.push(cert);
        let content = pem::encode_many_config(
            &[
                Pem::new("CERTIFICATE", cert_der),
                Pem::new("PRIVATE KEY", key_der),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        );
        fs::write(path, content).unwrap();

        let store = super::best_effort_root_store(paths).await.unwrap();
        assert_eq!(store.len(), certs.len());
    }
}
