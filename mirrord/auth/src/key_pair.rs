use std::{borrow::Cow, ops::Deref, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use x509_certificate::{InMemorySigningKeyPair, KeyAlgorithm, X509CertificateError};

/// Wrapper over [`InMemorySigningKeyPair`].
///
/// Can be (de)serialized from/to either valid or buggy format. The format can also be switched in
/// memory with [`Self::bug_der`] and [`Self::fix_der`]. See <https://github.com/briansmith/ring/issues/1464>.
#[derive(Debug, Clone)]
pub struct KeyPair {
    /// PEM-encoded document containing the key pair.
    pem: String,
    /// Whether the DER-encoded key pair container in [`Self::pem`] is in buggy format.
    der_bugged: bool,
    /// Deserialized and initialized key pair for signing.
    /// The key pair is wrapped in [`Arc`] only because [`InMemorySigningKeyPair`] is not
    /// cloneable.
    key_pair: Arc<InMemorySigningKeyPair>,
}

impl KeyPair {
    /// Buggy prefix of DER encoding used in old version `ring` crate.
    const BUGGED_PREFIX: [u8; 16] = [
        0x30, 0x53, 0x02, 0x01, 0x01, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];

    /// Buggy middle part of DER encoding used in old version of `ring` crate.
    const BUGGED_MIDDLE: [u8; 5] = [0xa1, 0x23, 0x03, 0x21, 0x00];

    /// Valid prefix of DER encoding, used for patching buggy DERs.
    const FIXED_PREFIX: [u8; 16] = [
        0x30, 0x51, 0x02, 0x01, 0x01, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];

    /// Valid middle part of DER encoding, used for patching buggy DERs.
    const FIXED_MIDDLE: [u8; 3] = [0x81, 0x21, 0x00];

    /// If the given DER was produced by old version of `ring` crate, returns a patched valid
    /// version that contains the same key pair. If not, returns [`None`].
    fn patch_buggy_der(der: &[u8]) -> Option<Vec<u8>> {
        if der.len() != 85
            || der.get(..16).expect("length was checked") != Self::BUGGED_PREFIX
            || der.get(16 + 32..16 + 32 + 5).expect("length was checked") != Self::BUGGED_MIDDLE
        {
            return None;
        }

        let seed = der
            .get(Self::BUGGED_PREFIX.len()..Self::BUGGED_PREFIX.len() + 32)
            .expect("length was checked");
        let public_key = der
            .get(Self::BUGGED_PREFIX.len() + 32 + Self::BUGGED_MIDDLE.len()..)
            .expect("length was checked");

        [&Self::FIXED_PREFIX, seed, &Self::FIXED_MIDDLE, public_key]
            .concat()
            .into()
    }

    /// Generates a new random [`KeyPair`].
    /// The new [`KeyPair`] initially has valid format.
    pub fn new_random() -> Result<Self, X509CertificateError> {
        let key_pair = InMemorySigningKeyPair::generate_random(KeyAlgorithm::Ed25519)?;
        Ok(key_pair.into())
    }

    /// Exposes this key pair as a PEM-encoded document.
    pub fn document(&self) -> &str {
        &self.pem
    }

    /// Changes format to the buggy one.
    /// Old `ring` and [`x509_certificate`] versions will accept it.
    /// New `ring` and [`x509_certificate`] versions will reject it.
    pub fn bug_der(&mut self) {
        if self.der_bugged {
            return;
        }

        let pem_key = pem::parse(&self.pem).expect("PEM was verified when creating this KeyPair");
        let der = pem_key.contents();
        let seed = der
            .get(Self::FIXED_PREFIX.len()..Self::FIXED_PREFIX.len() + 32)
            .expect("PEM was verified when creating this KeyPair");
        let public_key = der
            .get(Self::FIXED_PREFIX.len() + 32 + Self::FIXED_MIDDLE.len()..)
            .expect("PEM was verified when creating this KeyPair");

        let bugged_der = [&Self::BUGGED_PREFIX, seed, &Self::BUGGED_MIDDLE, public_key].concat();
        let pem_key = pem::Pem::new("PRIVATE KEY", bugged_der);
        let pem_document = pem::encode(&pem_key);

        self.pem = pem_document;
        self.der_bugged = true;
    }

    /// Changes format to the valid one.
    /// Old `ring` and [`x509_certificate`] versions will reject it.
    /// New `ring` and [`x509_certificate`] versions will accept it.
    pub fn fix_der(&mut self) {
        if !self.der_bugged {
            return;
        }

        let der = self.key_pair.to_pkcs8_one_asymmetric_key_der();
        let pem_key = pem::Pem::new("PRIVATE KEY", der.to_vec());
        let pem_document = pem::encode(&pem_key);

        self.pem = pem_document;
        self.der_bugged = false;
    }
}

impl From<InMemorySigningKeyPair> for KeyPair {
    fn from(key_pair: InMemorySigningKeyPair) -> Self {
        let der = key_pair.to_pkcs8_one_asymmetric_key_der();
        let pem_key = pem::Pem::new("PRIVATE KEY", der.to_vec());
        let pem_document = pem::encode(&pem_key);

        Self {
            pem: pem_document,
            key_pair: key_pair.into(),
            der_bugged: false,
        }
    }
}

impl Deref for KeyPair {
    type Target = InMemorySigningKeyPair;

    fn deref(&self) -> &Self::Target {
        &self.key_pair
    }
}

impl TryFrom<String> for KeyPair {
    type Error = X509CertificateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let pem_key = pem::parse(&value).map_err(X509CertificateError::PemDecode)?;
        let (contents, der_bugged) = match Self::patch_buggy_der(pem_key.contents()) {
            Some(contents) => (Cow::Owned(contents), true),
            None => (Cow::Borrowed(pem_key.contents()), false),
        };

        let key_pair = InMemorySigningKeyPair::from_pkcs8_der(contents)?;

        Ok(Self {
            pem: value,
            key_pair: key_pair.into(),
            der_bugged,
        })
    }
}

impl Serialize for KeyPair {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.pem.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for KeyPair {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let pem = String::deserialize(deserializer)?;
        Self::try_from(pem).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod test {
    use x509_certificate::Signer;

    use super::KeyPair;

    /// Verifies that [`KeyPair`] properly deserializes from old buggy format.
    #[test]
    fn deserialize_old_format() {
        // Produced with previous version of this crate.
        const SERIALIZED: &str = "-----BEGIN PRIVATE KEY-----\r\nMFMCAQEwBQYDK2VwBCIEIAnnKqvgSX5b4p2WZhe/hQOpt/D7z4P1H9UHJ2iiIat1\r\noSMDIQCQaTis0CQ62Y8+pePb3+x7umYRY0368BNyD5UrLZCMqA==\r\n-----END PRIVATE KEY-----\r\n";
        const EXPECTED_SIGNATURE: &[u8] = &[
            138, 4, 156, 91, 93, 73, 133, 216, 66, 25, 175, 249, 20, 105, 24, 28, 39, 28, 188, 63,
            249, 207, 106, 200, 98, 81, 184, 66, 241, 182, 24, 77, 2, 112, 208, 30, 189, 192, 138,
            69, 77, 143, 244, 61, 250, 18, 241, 254, 230, 160, 250, 208, 66, 217, 124, 86, 186,
            188, 139, 24, 152, 16, 185, 4,
        ];
        const MESSAGE_TO_SIGN: &[u8] = b"hello";

        // Verify that we're able to deserialize.
        let key_pair: KeyPair = serde_yaml::from_str(SERIALIZED).unwrap();
        assert!(key_pair.der_bugged);

        // Verify that signature is the same - we deserialized the exact same key pair.
        let signature = key_pair.sign(MESSAGE_TO_SIGN);
        assert_eq!(signature.as_ref(), EXPECTED_SIGNATURE);
    }

    /// Verifies that switching [`KeyPair`] format works fine - key pair is not changed and we're
    /// de(serializing) correct variant.
    #[test]
    fn format_conversion() {
        const MESSAGE_TO_SIGN: &[u8] = b"hello there";

        let key_pair = KeyPair::new_random().unwrap();
        let expected_signature = key_pair.sign(MESSAGE_TO_SIGN);

        // Serialize and deserialize without changing format, check key pair identity.
        assert!(!key_pair.der_bugged);
        let serialized = serde_yaml::to_string(&key_pair).unwrap();
        let mut deserialized: KeyPair = serde_yaml::from_str(&serialized).unwrap();
        assert!(!deserialized.der_bugged);
        let signature = deserialized.sign(MESSAGE_TO_SIGN);
        assert_eq!(signature.as_ref(), expected_signature.as_ref());

        // Switch to bugged format, serialize and deserialize. Check key pair identity.
        deserialized.bug_der();
        assert!(deserialized.der_bugged);
        let serialized = serde_yaml::to_string(&deserialized).unwrap();
        let mut deserialized: KeyPair = serde_yaml::from_str(&serialized).unwrap();
        assert!(deserialized.der_bugged);
        let signature = deserialized.sign(MESSAGE_TO_SIGN);
        assert_eq!(signature.as_ref(), expected_signature.as_ref());

        // Switch back to fixed format, serialize and deserialize. Check key pair identity.
        deserialized.fix_der();
        assert!(!deserialized.der_bugged);
        let serialized = serde_yaml::to_string(&deserialized).unwrap();
        let deserialized: KeyPair = serde_yaml::from_str(&serialized).unwrap();
        assert!(!deserialized.der_bugged);
        let signature = deserialized.sign(MESSAGE_TO_SIGN);
        assert_eq!(signature.as_ref(), expected_signature.as_ref());
    }
}
