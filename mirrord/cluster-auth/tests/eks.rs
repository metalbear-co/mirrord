//! EKS startup retries and token rotation through mock ECS credentials and HTTPS API servers.
//!
//! Kept in its own test binary: it sets process environment variables.

#![cfg(feature = "eks")]

mod common;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{SeenAuthorizations, start_server};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use mirrord_cluster_auth::{
    AuthMethod, ClusterAuthError, ClusterClientFactory, ClusterCredentials,
};
use tokio::net::TcpListener;

#[tokio::test]
async fn eks_connection_sends_and_refreshes_an_eks_token() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let credential_requests = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let credentials_url = format!("http://{}/credentials", listener.local_addr().unwrap());
    let requests = credential_requests.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let requests = requests.clone();
            tokio::spawn(async move {
                let service = service_fn(move |_| {
                    let attempt = requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let response = if !(2..1000).contains(&attempt) {
                            Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .body(Full::new(Bytes::from_static(b"temporary failure")))
                                .unwrap()
                        } else {
                            Response::new(Full::new(Bytes::from(format!(
                                "{{\"AccessKeyId\":\"AKID{attempt}\",\"SecretAccessKey\":\"secret\",\"Token\":\"session-token\",\"Expiration\":\"2100-01-01T00:00:00Z\"}}"
                            ))))
                        };
                        Ok::<_, Infallible>(response)
                    }
                });
                http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                    .unwrap();
            });
        }
    });
    // SAFETY: the only test in this binary, set before anything reads the environment.
    unsafe {
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::remove_var("AWS_SESSION_TOKEN");
        std::env::remove_var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI");
        std::env::set_var("AWS_CONTAINER_CREDENTIALS_FULL_URI", credentials_url);
        std::env::set_var("AWS_PROFILE", "mirrord-cluster-auth-test-no-profile");
        std::env::set_var("AWS_MAX_ATTEMPTS", "1");
        std::env::set_var("AWS_REGION", "us-east-1");
        std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    }
    let seen = SeenAuthorizations::default();
    let (address, certificate_pem) = start_server(seen.clone()).await;
    let factory = ClusterClientFactory::new().unwrap();

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
    assert_eq!(credential_requests.load(Ordering::SeqCst), 3);

    connection
        .client
        .request_text(Request::get("/version").body(Vec::new()).unwrap())
        .await
        .unwrap();
    let authorization = seen.lock().unwrap().first().unwrap().clone().unwrap();
    assert!(authorization.starts_with("Bearer k8s-aws-v1."));

    let token_file_path = connection.token_file_path.clone().unwrap();
    let first_token = connection.credentials.token.clone().unwrap();
    let first_expiry = connection.token_expiry.unwrap();
    let mut other_credentials = connection.credentials.clone();
    other_credentials.token = Some("other-token".to_owned());
    let other_client = factory.build_client(&other_credentials).await.unwrap();

    factory.refresh_token(&mut connection).await.unwrap();

    let refreshed = std::fs::read_to_string(&token_file_path).unwrap();
    assert_ne!(refreshed, first_token);
    assert!(refreshed.starts_with("k8s-aws-v1."));
    assert_eq!(
        connection.credentials.token.as_deref(),
        Some(refreshed.as_str())
    );
    assert!(connection.token_expiry.unwrap() >= first_expiry);

    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::metadata(factory.token_files_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&token_file_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    tokio::time::sleep(Duration::from_secs(61)).await;
    connection
        .client
        .request_text(Request::get("/version").body(Vec::new()).unwrap())
        .await
        .unwrap();
    other_client
        .request_text(Request::get("/version").body(Vec::new()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            Some(format!("Bearer {first_token}")),
            Some(format!("Bearer {refreshed}")),
            Some("Bearer other-token".to_owned()),
        ]
    );

    let mut broken_connection = connection.clone();
    broken_connection.token_expiry = Some(SystemTime::now());
    broken_connection.token_file_path = Some(factory.token_files_dir().join("missing/token"));
    assert!(matches!(
        factory.run_token_refresh(broken_connection).await,
        Err(ClusterAuthError::Internal(..))
    ));

    let mut expired_connection = connection.clone();
    let valid_credentials = connection.credentials.clone();
    connection.token_expiry = Some(SystemTime::now());
    connection.credentials.auth_method = AuthMethod::AwsIam {
        region: "invalid region".to_owned(),
        cluster_name: "my-cluster".to_owned(),
    };
    assert!(matches!(
        factory.run_token_refresh(connection).await,
        Err(ClusterAuthError::ConfigError(..))
    ));

    credential_requests.store(1000, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            factory.connect_eks(valid_credentials),
        )
        .await
        .is_err()
    );
    let requests_after_cancellation = credential_requests.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        credential_requests.load(Ordering::SeqCst),
        requests_after_cancellation
    );

    // Credentials stay unavailable past the token's expiry, so refresh gives up instead of
    // retrying with a token the API server rejects.
    expired_connection.token_expiry = Some(SystemTime::now() - Duration::from_secs(1));
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            factory.run_token_refresh(expired_connection),
        )
        .await
        .expect("refresh must give up once the token has expired"),
        Err(ClusterAuthError::CredentialsUnavailable(..))
    ));
}
