use std::{fmt::Debug, future::Future};

use futures::{future, FutureExt};
use kube::{api::PostParams, Api, Client, Resource};
use serde::Deserialize;
#[cfg(feature = "client")]
use x509_certificate::{
    rfc2986, InMemorySigningKeyPair, X509CertificateBuilder, X509CertificateError,
};

use crate::{certificate::Certificate, error::CredentialStoreError, key_pair::KeyPair};

pub trait AuthClient<R> {
    fn obtain_certificate(
        &self,
        key_pair: &KeyPair,
        common_name: &str,
    ) -> impl Future<Output = Result<Certificate, CredentialStoreError>> + Send;

}

impl<R> AuthClient<R> for Client
where
    R: for<'de> Deserialize<'de> + Resource + Clone + Debug,
    <R as Resource>::DynamicType: Default,
{
    fn obtain_certificate(
        &self,
        key_pair: &KeyPair,
        common_name: &str,
    ) -> impl Future<Output = Result<Certificate, CredentialStoreError>> + Send {
        let certificate_request = match certificate_request(common_name, &key_pair)
            .and_then(|req| req.encode_pem().map_err(X509CertificateError::from))
        {
            Ok(req) => req,
            Err(e) => return future::err(e.into()).boxed(),
        };

        async move {
            let api: Api<R> = Api::all(self.clone());

            api.create_subresource(
                "certificate",
                "operator",
                &PostParams::default(),
                certificate_request.into(),
            )
            .await
            .map_err(CredentialStoreError::from)
        }
        .boxed()
    }
}

fn certificate_request(
    common_name: &str,
    key_pair: &InMemorySigningKeyPair,
) -> Result<rfc2986::CertificationRequest, X509CertificateError> {
    let mut builder = X509CertificateBuilder::default();

    let _ = builder
        .subject()
        .append_common_name_utf8_string(common_name);

    builder.create_certificate_signing_request(key_pair)
}
