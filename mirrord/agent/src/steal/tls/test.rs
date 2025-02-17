use std::{fs, path::Path, sync::Arc};

use mirrord_agent_env::steal_tls::{
    AgentClientConfig, AgentServerConfig, StealPortTlsConfig, TlsAuthentication,
    TlsClientVerification, TlsServerVerification,
};
use pem::{EncodeConfig, LineEnding, Pem};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedKey, DnType, DnValue, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
    ClientConfig, RootCertStore,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::{steal::StealTlsHandlerStore, util::path_resolver::InTargetPathResolver};

/// Generates a new [`CertifiedKey`] with a random [`KeyPair`].
fn generate_cert(
    name: String,
    issuer: Option<&CertifiedKey>,
    can_sign_others: bool,
) -> CertifiedKey {
    let key_pair = KeyPair::generate().unwrap();

    let mut params = CertificateParams::new(vec![name.clone()]).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, DnValue::Utf8String(name.clone()));

    if can_sign_others {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    }

    let cert = match issuer {
        Some(issuer) => params
            .signed_by(&key_pair, &issuer.cert, &issuer.key_pair)
            .unwrap(),
        None => params.self_signed(&key_pair).unwrap(),
    };

    CertifiedKey { cert, key_pair }
}

struct CertChainWithKey {
    key: PrivateKeyDer<'static>,
    certs: Vec<CertificateDer<'static>>,
}

impl CertChainWithKey {
    fn new(end_entity_name: String, root_cert: Option<&CertifiedKey>) -> Self {
        let mut new_root = None;

        let root = match root_cert {
            Some(cert) => cert,
            None => {
                new_root.replace(generate_cert("root".into(), None, true));
                new_root.as_ref().unwrap()
            }
        };

        let issuer = generate_cert("issuer".into(), Some(root), true);
        let cert = generate_cert(end_entity_name, Some(&issuer), false);

        Self {
            key: cert.key_pair.serialize_der().try_into().unwrap(),
            certs: vec![
                cert.cert.into(),
                issuer.cert.into(),
                root.cert.der().clone(),
            ],
        }
    }

    fn to_file(&self, path: &Path) {
        let mut pems = Vec::with_capacity(self.certs.len() + 1);

        pems.push(Pem::new("PRIVATE KEY", self.key.secret_der()));
        for cert in &self.certs {
            pems.push(Pem::new("CERTIFICATE", cert.as_ref()));
        }

        let content =
            pem::encode_many_config(&pems, EncodeConfig::new().set_line_ending(LineEnding::LF));
        fs::write(path, content).unwrap();
    }
}

async fn assert_can_talk(acceptor: TlsAcceptor, connector: TlsConnector, server_name: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    let server_name = ServerName::try_from(server_name.to_string()).unwrap();

    tokio::spawn(async move {
        let stream = TcpStream::connect(server_addr).await.unwrap();
        let mut stream = connector.connect(server_name, stream).await.unwrap();

        stream.write_all(b"hello there").await.unwrap();
        stream.shutdown().await.unwrap();
    });

    let stream = listener.accept().await.unwrap().0;
    let mut stream = acceptor.accept(stream).await.unwrap();

    let mut message = String::new();
    stream.read_to_string(&mut message).await.unwrap();
    assert_eq!(message, "hello there");
}

async fn assert_cannot_talk(acceptor: TlsAcceptor, connector: TlsConnector, server_name: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    let server_name = ServerName::try_from(server_name.to_string()).unwrap();

    tokio::spawn(async move {
        let stream = TcpStream::connect(server_addr).await.unwrap();
        let mut stream = connector.connect(server_name, stream).await.unwrap();

        stream.write_all(b"hello there").await.unwrap();
        stream.shutdown().await.unwrap();
    });

    let stream = listener.accept().await.unwrap().0;
    acceptor.accept(stream).await.unwrap_err();
}

/// Verifies that agent's TLS server correctly authenticates itself to the clients.
#[tokio::test]
async fn server_authentication() {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let root_dir = tempfile::tempdir().unwrap();
    let server_pem = root_dir.path().join("auth.pem");

    let chain = CertChainWithKey::new("mirrord-agent".into(), None);
    chain.to_file(&server_pem);

    let connector = {
        let mut client_root_store = RootCertStore::empty();
        client_root_store
            .add(chain.certs.last().unwrap().clone())
            .unwrap();
        let client_config = ClientConfig::builder()
            .with_root_certificates(client_root_store)
            .with_no_client_auth();
        TlsConnector::from(Arc::new(client_config))
    };

    let store = StealTlsHandlerStore::new(
        vec![StealPortTlsConfig {
            port: 443,
            agent_as_server: AgentServerConfig {
                authentication: TlsAuthentication {
                    cert_pem: "/auth.pem".into(),
                    key_pem: "/auth.pem".into(),
                },
                alpn_protocols: Default::default(),
                verification: None,
            },
            agent_as_client: AgentClientConfig {
                authentication: None,
                verification: TlsServerVerification {
                    accept_any_cert: true,
                    trust_roots: Default::default(),
                },
            },
        }],
        InTargetPathResolver::with_root_path(root_dir.path().to_path_buf()),
    );
    let handler = store.get(443).await.unwrap().unwrap();

    assert_can_talk(handler.acceptor(), connector, "mirrord-agent").await;
}

/// Verifies that agent's TLS server correctly verifies clients.
#[rstest::rstest]
#[case::known_root_accepted(false, true, false, false, true)]
#[case::anonymous_client_rejected(true, false, false, false, false)]
#[case::unknown_root_rejected(false, false, false, false, false)]
#[case::anonymous_client_accepted_with_allow_anonymous(true, false, true, false, true)]
#[case::unknown_cert_accepted_with_accept_any_cert(false, false, false, true, true)]
#[tokio::test]
async fn client_verification(
    #[case] anonymous_client: bool,
    #[case] good_client_root: bool,
    #[case] allow_anonymous: bool,
    #[case] accept_any_cert: bool,
    #[case] expect_client_accepted: bool,
) {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let root_dir = tempfile::tempdir().unwrap();

    let chain = CertChainWithKey::new("mirrord-agent".into(), None);
    let auth_pem = root_dir.path().join("auth.pem");
    chain.to_file(&auth_pem);

    let trusted_root = generate_cert("root".into(), None, true);
    let root_pem = root_dir.path().join("root.pem");
    fs::write(&root_pem, trusted_root.cert.pem()).unwrap();

    let connector = {
        let mut root_store = RootCertStore::empty();
        root_store.add(chain.certs.last().unwrap().clone()).unwrap();
        let builder = ClientConfig::builder().with_root_certificates(root_store);
        let config = if anonymous_client {
            builder.with_no_client_auth()
        } else if good_client_root {
            let cert_chain = CertChainWithKey::new("client".into(), Some(&trusted_root));
            builder
                .with_client_auth_cert(cert_chain.certs, cert_chain.key)
                .unwrap()
        } else {
            let cert_chain = CertChainWithKey::new("client".into(), None);
            builder
                .with_client_auth_cert(cert_chain.certs, cert_chain.key)
                .unwrap()
        };
        TlsConnector::from(Arc::new(config))
    };

    let store = StealTlsHandlerStore::new(
        vec![StealPortTlsConfig {
            port: 443,
            agent_as_server: AgentServerConfig {
                authentication: TlsAuthentication {
                    cert_pem: "/auth.pem".into(),
                    key_pem: "/auth.pem".into(),
                },
                alpn_protocols: Default::default(),
                verification: Some(TlsClientVerification {
                    allow_anonymous,
                    accept_any_cert,
                    trust_roots: vec!["/root.pem".into()],
                }),
            },
            agent_as_client: AgentClientConfig {
                authentication: None,
                verification: TlsServerVerification {
                    accept_any_cert: true,
                    trust_roots: Default::default(),
                },
            },
        }],
        InTargetPathResolver::with_root_path(root_dir.path().to_path_buf()),
    );
    let handler = store.get(443).await.unwrap().unwrap();
    println!("handler: {handler:?}");

    if expect_client_accepted {
        assert_can_talk(handler.acceptor(), connector, "mirrord-agent").await;
    } else {
        assert_cannot_talk(handler.acceptor(), connector, "mirrord-agent").await;
    }
}
