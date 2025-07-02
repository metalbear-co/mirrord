use std::{fmt::Debug, future::Future};

use futures::{future, FutureExt};
use kube::{api::PostParams, Api, Client, Resource};
use serde::Deserialize;
#[cfg(feature = "client")]
use x509_certificate::{
    rfc2986, InMemorySigningKeyPair, X509CertificateBuilder, X509CertificateError,
};

use crate::{certificate::Certificate, error::CredentialStoreError, key_pair::KeyPair};

#[cfg(feature = "client")]
pub trait AuthClient<R> {
    fn obtain_certificate(
        &self,
        key_pair: &KeyPair,
        common_name: &str,
    ) -> impl Future<Output = Result<Certificate, CredentialStoreError>> + Send;
}

#[cfg(feature = "client")]
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
        let certificate_request = match certificate_request(common_name, key_pair)
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

#[cfg(test)]
pub mod client_mock {
    use std::{
        collections::VecDeque,
        fmt::Debug,
        sync::{Arc, Mutex},
    };

    use kube::Resource;
    use serde::Deserialize;

    use crate::{
        certificate::Certificate, error::CredentialStoreError, key_pair::KeyPair, AuthClient,
    };

    #[derive(Clone, Debug, Default)]
    pub struct ClientMock {
        pub return_error: bool,
        pub calls_list: Arc<Mutex<VecDeque<(KeyPair, String)>>>,
    }

    impl<R> AuthClient<R> for ClientMock
    where
        R: for<'de> Deserialize<'de> + Resource + Clone + Debug,
        <R as Resource>::DynamicType: Default,
    {
        async fn obtain_certificate(
            &self,
            key_pair: &KeyPair,
            common_name: &str,
        ) -> Result<Certificate, CredentialStoreError> {
            self.calls_list
                .lock()
                .unwrap()
                .push_front((key_pair.clone(), common_name.to_string()));
            if self.return_error {
                Err(CredentialStoreError::FileAccess(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Mock Error",
                )))
            } else {
                Ok(certificate_mock())
            }
        }
    }

    impl ClientMock {
        pub fn pop_last_call(&self) -> Option<(KeyPair, String)> {
            self.calls_list.lock().unwrap().pop_back()
        }
    }

    pub fn certificate_mock() -> Certificate {
        const SERIALIZED: &str = r#""-----BEGIN CERTIFICATE-----\r\nMIICGTCCAcmgAwIBAgIBATAHBgMrZXAFADBwMUIwQAYDVQQDDDlUaGUgTWljaGHF\r\ngiBTbW9sYXJlayBPcmdhbml6YXRpb25gcyBUZWFtcyBMaWNlbnNlIChUcmlhbCkx\r\nKjAoBgNVBAoMIVRoZSBNaWNoYcWCIFNtb2xhcmVrIE9yZ2FuaXphdGlvbjAeFw0y\r\nNDAyMDgxNTUwNDFaFw0yNDEyMjQwMDAwMDBaMBsxGTAXBgNVBAMMEHJheno0Nzgw\r\nLW1hY2hpbmUwLDAHBgMrZW4FAAMhAAfxTouyk5L5lB3eFwC5Rg9iI4KmQaFpnGVM\r\n2sYpv9HOo4HYMIHVMIHSBhcvbWV0YWxiZWFyL2xpY2Vuc2UvaW5mbwEB/wSBs3si\r\ndHlwZSI6InRlYW1zIiwibWF4X3NlYXRzIjpudWxsLCJzdWJzY3JpcHRpb25faWQi\r\nOiJmMWIxZDI2ZS02NGQzLTQ4YjYtYjVkMi05MzAxMzAwNWE3MmUiLCJvcmdhbml6\r\nYXRpb25faWQiOiIzNTdhZmE4MS0yN2QxLTQ3YjEtYTFiYS1hYzM1ZjlhM2MyNjMi\r\nLCJ0cmlhbCI6dHJ1ZSwidmVyc2lvbiI6IjMuNzMuMCJ9MAcGAytlcAUAA0EAJbbo\r\nu42KnHJBbPMYspMdv9ZdTQMixJgQUheNEs/o4+XfwgYOaRjCVQTzYs1m9f720WQ9\r\n4J04GdQvcu7B/oTgDQ==\r\n-----END CERTIFICATE-----\r\n""#;
        serde_yaml::from_str(SERIALIZED).unwrap()
    }
}
