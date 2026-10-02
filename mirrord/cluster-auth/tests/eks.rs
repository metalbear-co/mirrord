//! [`ClusterClientFactory::connect_eks`] and its token refresh, signing with static AWS
//! credentials from the environment.
//!
//! Kept in its own test binary: it sets process environment variables.

#![cfg(feature = "eks")]

mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{SeenAuthorizations, start_server};
use hyper::Request;
use mirrord_cluster_auth::{AuthMethod, ClusterClientFactory, ClusterCredentials};

#[tokio::test]
async fn eks_connection_sends_and_refreshes_an_eks_token() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    // SAFETY: the only test in this binary, set before anything reads the environment.
    unsafe {
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("AWS_REGION", "us-east-1");
        std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    }
    let seen = SeenAuthorizations::default();
    let (address, certificate_pem) = start_server(seen.clone()).await;
    let factory = ClusterClientFactory::new();

    let mut connection = factory
        .connect_eks(ClusterCredentials {
            name: format!("eks-{}", address.port()),
            server: format!("https://localhost:{}", address.port()),
            namespace: "default".to_owned(),
            ca_data: Some(STANDARD.encode(&certificate_pem)),
            auth_method: AuthMethod::AwsIam {
                region: "us-east-1".to_owned(),
                cluster_name: "my-cluster".to_owned(),
            },
            token: None,
            client_cert_pem: None,
            client_key_pem: None,
        })
        .await
        .unwrap();

    connection
        .client
        .request_text(Request::get("/version").body(Vec::new()).unwrap())
        .await
        .unwrap();
    let authorization = seen.lock().unwrap()[0].clone().unwrap();
    assert!(authorization.starts_with("Bearer k8s-aws-v1."));

    let token_file_path = connection.token_file_path.clone().unwrap();
    let first_expiry = connection.token_expiry.unwrap();
    std::fs::remove_file(&token_file_path).unwrap();

    factory.refresh_token(&mut connection).await.unwrap();

    let refreshed = std::fs::read_to_string(&token_file_path).unwrap();
    assert!(refreshed.starts_with("k8s-aws-v1."));
    assert_eq!(
        connection.credentials.token.as_deref(),
        Some(refreshed.as_str())
    );
    assert!(connection.token_expiry.unwrap() >= first_expiry);
}
