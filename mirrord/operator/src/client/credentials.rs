use kube::api::ObjectMeta;
use mirrord_auth::{
    certificate::Certificate,
    credentials::client::{SigningRequest, SigningResponse},
    error::CredentialStoreError,
    x509_certificate::X509Certificate,
};

use crate::crd;

impl SigningRequest for crd::MirrordClusterOperatorUserCredential {
    fn regular(csr: String) -> Self {
        let spec = crd::MirrordClusterOperatorUserCredentialSpec {
            csr,
            kind: crd::UserCredentialKind::Regular,
        };
        Self {
            metadata: ObjectMeta {
                generate_name: Some("mirrord-operator-regular-cred-".to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        }
    }

    fn ci(csr: String) -> Self {
        let spec = crd::MirrordClusterOperatorUserCredentialSpec {
            csr,
            kind: crd::UserCredentialKind::Ci,
        };
        Self {
            metadata: ObjectMeta {
                generate_name: Some("mirrord-operator-ci-cred-".to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        }
    }
}

impl SigningResponse for crd::MirrordClusterOperatorUserCredential {
    fn try_into_certificate(self) -> Result<Certificate, CredentialStoreError> {
        let certificate = X509Certificate::from_pem(
            self.status
                .ok_or(CredentialStoreError::SigningResponse)?
                .certificate,
        )?;

        Ok(certificate.into())
    }
}
