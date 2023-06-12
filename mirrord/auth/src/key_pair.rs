use std::{ops::Deref, sync::OnceLock};

use serde::{Deserialize, Serialize};
use x509_certificate::InMemorySigningKeyPair;

/// Wrapps `InMemorySigningKeyPair` & the underlying pkcs8 formatted key
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyPair(String, #[serde(skip)] OnceLock<InMemorySigningKeyPair>);

impl Deref for KeyPair {
    type Target = InMemorySigningKeyPair;

    fn deref(&self) -> &Self::Target {
        self.1.get_or_init(|| {
            InMemorySigningKeyPair::from_pkcs8_pem(&self.0).expect("Invalid pkcs8 key stored")
        })
    }
}

impl From<&str> for KeyPair {
    fn from(key: &str) -> Self {
        KeyPair(key.to_owned(), Default::default())
    }
}

impl From<String> for KeyPair {
    fn from(key: String) -> Self {
        KeyPair(key, Default::default())
    }
}
