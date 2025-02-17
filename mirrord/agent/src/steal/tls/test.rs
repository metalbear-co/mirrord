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
    server::WebPkiClientVerifier,
    ClientConfig, RootCertStore, ServerConfig,
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
#[rstest::rstest]
#[case::client_trusts_agent_root(true, true)]
#[case::client_does_not_trust_agent_root(false, false)]
#[tokio::test]
async fn server_authentication(
    #[case] client_trusts_agent_root: bool,
    #[case] expect_accepted: bool,
) {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let root_dir = tempfile::tempdir().unwrap();
    let server_pem = root_dir.path().join("auth.pem");

    let chain = CertChainWithKey::new("mirrord-agent".into(), None);
    chain.to_file(&server_pem);

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

    let connector = {
        let mut root_store = RootCertStore::empty();

        if client_trusts_agent_root {
            root_store.add(chain.certs.last().unwrap().clone()).unwrap();
        } else {
            let cert = generate_cert("dummy root".into(), None, true);
            root_store.add(cert.cert.into()).unwrap();
        }

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        TlsConnector::from(Arc::new(client_config))
    };

    if expect_accepted {
        assert_can_talk(handler.acceptor(), connector, "mirrord-agent").await;
    } else {
        assert_cannot_talk(handler.acceptor(), connector, "mirrord-agent").await;
    }
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

    if expect_client_accepted {
        assert_can_talk(handler.acceptor(), connector, "mirrord-agent").await;
    } else {
        assert_cannot_talk(handler.acceptor(), connector, "mirrord-agent").await;
    }
}

/// Verifies that agent's TLS client correctly authenticates itself to the server,
/// when configured.
#[rstest::rstest]
#[case::server_trusts_agent_root(true, true)]
#[case::server_does_not_trust_agent_root(false, false)]
#[tokio::test]
async fn client_authentication(
    #[case] server_trusts_agent_root: bool,
    #[case] expect_accepted: bool,
) {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let root_dir = tempfile::tempdir().unwrap();

    let agent_chain = CertChainWithKey::new("mirrord-agent".into(), None);
    let auth_pem = root_dir.path().join("auth.pem");
    agent_chain.to_file(&auth_pem);

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
                authentication: Some(TlsAuthentication {
                    cert_pem: "/auth.pem".into(),
                    key_pem: "/auth.pem".into(),
                }),
                verification: TlsServerVerification {
                    accept_any_cert: true,
                    trust_roots: Default::default(),
                },
            },
        }],
        InTargetPathResolver::with_root_path(root_dir.path().to_path_buf()),
    );
    let handler = store.get(443).await.unwrap().unwrap();

    let acceptor = {
        let mut root_store = RootCertStore::empty();

        if server_trusts_agent_root {
            root_store
                .add(agent_chain.certs.last().unwrap().clone())
                .unwrap();
        } else {
            let cert = generate_cert("dummy root".into(), None, true);
            root_store.add(cert.cert.into()).unwrap();
        }

        let verifier = WebPkiClientVerifier::builder(root_store.into())
            .build()
            .unwrap();
        let server_chain = CertChainWithKey::new("server".into(), None);
        let config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(server_chain.certs, server_chain.key)
            .unwrap();
        TlsAcceptor::from(Arc::new(config))
    };

    let connector = TlsConnector::from(handler.client_config);

    if expect_accepted {
        assert_can_talk(acceptor, connector, "server").await;
    } else {
        assert_cannot_talk(acceptor, connector, "server").await;
    }
}

/// Verifies that agent's TLS client correctly verifies the server.
#[rstest::rstest]
#[case::accept_any_cert_trusted(true, false, true)]
#[case::accept_any_cert_not_trusted(true, true, true)]
#[case::trusted(false, true, true)]
#[case::not_trusted(false, false, false)]
#[tokio::test]
async fn server_verification(
    #[case] accept_any_cert: bool,
    #[case] server_uses_trusted_root: bool,
    #[case] expect_accepted: bool,
) {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let root_dir = tempfile::tempdir().unwrap();

    let agent_chain = CertChainWithKey::new("mirrord-agent".into(), None);
    let auth_pem = root_dir.path().join("auth.pem");
    agent_chain.to_file(&auth_pem);

    let trusted_root = generate_cert("root".into(), None, true);
    let root_pem = root_dir.path().join("root.pem");
    fs::write(root_pem, trusted_root.cert.pem()).unwrap();

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
                authentication: Some(TlsAuthentication {
                    cert_pem: "/auth.pem".into(),
                    key_pem: "/auth.pem".into(),
                }),
                verification: TlsServerVerification {
                    accept_any_cert,
                    trust_roots: vec!["/root.pem".into()],
                },
            },
        }],
        InTargetPathResolver::with_root_path(root_dir.path().to_path_buf()),
    );
    let handler = store.get(443).await.unwrap().unwrap();

    let acceptor = {
        let mut root_store = RootCertStore::empty();
        root_store
            .add(agent_chain.certs.last().unwrap().clone())
            .unwrap();
        let verifier = WebPkiClientVerifier::builder(root_store.into())
            .build()
            .unwrap();
        let issuer = server_uses_trusted_root.then_some(&trusted_root);
        let server_chain = CertChainWithKey::new("server".into(), issuer);
        let config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(server_chain.certs, server_chain.key)
            .unwrap();
        TlsAcceptor::from(Arc::new(config))
    };

    let connector = TlsConnector::from(handler.client_config);

    if expect_accepted {
        assert_can_talk(acceptor, connector, "server").await;
    } else {
        assert_cannot_talk(acceptor, connector, "server").await;
    }
}

/// Verifies that agent uses original SNI and ALPN protocol when making a passthrough connection.
#[tokio::test]
async fn agent_connects_with_original_params() {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let root_dir = tempfile::tempdir().unwrap();

    let trusted_root = generate_cert("root".into(), None, true);
    let root_pem = root_dir.path().join("root.pem");
    fs::write(root_pem, trusted_root.cert.pem()).unwrap();

    let agent_chain = CertChainWithKey::new("server".into(), Some(&trusted_root));
    let auth_pem = root_dir.path().join("auth.pem");
    agent_chain.to_file(&auth_pem);

    let store = StealTlsHandlerStore::new(
        vec![StealPortTlsConfig {
            port: 443,
            agent_as_server: AgentServerConfig {
                authentication: TlsAuthentication {
                    cert_pem: "/auth.pem".into(),
                    key_pem: "/auth.pem".into(),
                },
                alpn_protocols: vec!["h2".into(), "http/1.1".into()],
                verification: Some(TlsClientVerification {
                    allow_anonymous: false,
                    accept_any_cert: false,
                    trust_roots: vec!["/root.pem".into()],
                }),
            },
            agent_as_client: AgentClientConfig {
                authentication: Some(TlsAuthentication {
                    cert_pem: "/auth.pem".into(),
                    key_pem: "/auth.pem".into(),
                }),
                verification: TlsServerVerification {
                    accept_any_cert: false,
                    trust_roots: vec!["/root.pem".into()],
                },
            },
        }],
        InTargetPathResolver::with_root_path(root_dir.path().to_path_buf()),
    );
    let handler = store.get(443).await.unwrap().unwrap();

    let mut root_store = RootCertStore::empty();
    root_store.add(trusted_root.cert.der().clone()).unwrap();
    let root_store = Arc::new(root_store);

    let connector = {
        let chain = CertChainWithKey::new("client".into(), Some(&trusted_root));
        let mut config = ClientConfig::builder()
            .with_root_certificates(root_store.clone())
            .with_client_auth_cert(chain.certs, chain.key)
            .unwrap();
        config.alpn_protocols.push(b"http/1.1".into());
        TlsConnector::from(Arc::new(config))
    };

    let acceptor = {
        let verifier = WebPkiClientVerifier::builder(root_store.into())
            .build()
            .unwrap();
        let server_chain = CertChainWithKey::new("server".into(), Some(&trusted_root));
        let mut config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(server_chain.certs, server_chain.key)
            .unwrap();
        config.alpn_protocols = vec![b"h2".into(), b"http/1.1".into()];
        TlsAcceptor::from(Arc::new(config))
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let _client_handle = tokio::spawn(async move {
        let client_agent = TcpStream::connect(addr).await.unwrap();
        connector
            .connect(
                ServerName::try_from("server".to_string()).unwrap(),
                client_agent,
            )
            .await
    });

    let agent_client = listener.accept().await.unwrap().0;
    let agent_client = handler.acceptor().accept(agent_client).await.unwrap();
    assert_eq!(
        agent_client.get_ref().1.alpn_protocol(),
        Some(b"http/1.1".as_slice())
    );
    assert_eq!(agent_client.get_ref().1.server_name(), Some("server"));

    let server_handle = tokio::spawn(async move {
        let server_agent = listener.accept().await.unwrap().0;
        acceptor.accept(server_agent).await.unwrap()
    });

    let agent_server = TcpStream::connect(addr).await.unwrap();
    let _agent_server = handler
        .connector(agent_client.get_ref().1)
        .connect(addr.ip(), None, agent_server)
        .await
        .unwrap();

    let server_agent = server_handle.await.unwrap();
    assert_eq!(
        server_agent.get_ref().1.alpn_protocol(),
        Some(b"http/1.1".as_slice())
    );
    assert_eq!(server_agent.get_ref().1.server_name(), Some("server"));
}
