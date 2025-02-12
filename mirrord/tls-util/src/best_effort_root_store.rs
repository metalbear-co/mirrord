use std::{fs::File, io::BufReader, path::Path, sync::Arc};

use rustls::RootCertStore;

use crate::BestEffortRootStoreError;

pub struct BestEffortRootStore {
    store: RootCertStore,
}

impl Default for BestEffortRootStore {
    fn default() -> Self {
        Self {
            store: RootCertStore::empty(),
        }
    }
}

impl BestEffortRootStore {
    pub fn try_add_all_from_pem(&mut self, path: &Path) {
        let Ok(pem) = File::open(path) else {
            return;
        };

        for cert in rustls_pemfile::certs(&mut BufReader::new(pem)) {
            let Ok(cert) = cert else {
                // Inspected `rustls_pemfile::certs` code
                // and it looks like we can hit an endless loop if we don't stop iterating.
                return;
            };

            let _ = self.store.add(cert);
        }
    }

    pub fn certs(&self) -> usize {
        self.store.len()
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
