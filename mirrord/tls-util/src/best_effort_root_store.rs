use std::{fs::File, io::BufReader, path::Path, sync::Arc};

use rustls::RootCertStore;

use crate::BestEffortRootStoreError;

pub struct BestEffortRootStore {
    store: RootCertStore,
    failed_files: usize,
    failed_certs: usize,
}

impl Default for BestEffortRootStore {
    fn default() -> Self {
        Self {
            store: RootCertStore::empty(),
            failed_certs: 0,
            failed_files: 0,
        }
    }
}

impl BestEffortRootStore {
    pub fn try_add_all_from_pem(&mut self, path: &Path) {
        let Ok(pem) = File::open(path) else {
            self.failed_files += 1;
            return;
        };

        for cert in rustls_pemfile::certs(&mut BufReader::new(pem)) {
            match cert {
                Ok(cert) => match self.store.add(cert) {
                    Ok(()) => {}
                    Err(..) => {
                        self.failed_certs += 1;
                    }
                },
                Err(..) => {
                    self.failed_files += 1;
                    // Inspected `rustls_pemfile::certs` code
                    // and it looks like we can hit an endless loop if we don't break.
                    continue;
                }
            }
        }
    }

    pub fn certs(&self) -> usize {
        self.store.len()
    }

    pub fn failed_certs(&self) -> usize {
        self.failed_certs
    }

    pub fn failed_files(&self) -> usize {
        self.failed_files
    }

    pub(crate) fn add_dummy(&mut self) -> Result<(), BestEffortRootStoreError> {
        let dummy = rcgen::generate_simple_self_signed(["dummy".to_string()])?;
        self.store.add(dummy.cert.into())?;

        Ok(())
    }
}

impl From<BestEffortRootStore> for RootCertStore {
    fn from(value: BestEffortRootStore) -> Self {
        value.store
    }
}

impl From<BestEffortRootStore> for Arc<RootCertStore> {
    fn from(value: BestEffortRootStore) -> Self {
        Arc::new(value.store)
    }
}
