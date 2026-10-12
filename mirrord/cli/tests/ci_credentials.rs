#![cfg(unix)]

use std::{ops::Not, process::Output, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    routing::any,
};
use base64::{Engine, prelude::BASE64_STANDARD};
use mirrord_auth::{
    certificate::Certificate,
    credentials::{CiApiKey, Credentials},
};
use mirrord_command::resolve_tokio_command;
use rcgen::{CertificateParams, CertificateSigningRequestParams, CertifiedIssuer, KeyPair};
use rstest::rstest;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle, time::timeout};

const OPERATOR_PATH: &str = "/apis/operator.metalbear.co/v1/mirrordoperators/operator";
const CREDENTIAL_PATH: &str =
    "/apis/operator.metalbear.co/v1/mirrordclusteroperatorusercredentials";
const TARGET_PATH: &str = "/apis/operator.metalbear.co/v1/namespaces/default/targets/targetless";
const ADMISSION_ERROR: &str = "CiCredentialFixtureAdmission";

#[derive(Default)]
struct Requests {
    operator: Value,
    operator_reads: usize,
    credential_kinds: Vec<String>,
    issued_certificate: Option<Vec<u8>>,
    connections: Vec<(Value, Vec<u8>)>,
    unexpected: Vec<String>,
}

async fn operator_request(
    State(requests): State<Arc<Mutex<Requests>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let mut requests = requests.lock().await;
    match (method, uri.path()) {
        (Method::GET, OPERATOR_PATH) => {
            requests.operator_reads += 1;
            (StatusCode::OK, Json(requests.operator.clone()))
        }
        (Method::POST, CREDENTIAL_PATH) => {
            let mut credential: Value = serde_json::from_slice(&body).unwrap();
            requests.credential_kinds.push(
                credential
                    .get("spec")
                    .unwrap()
                    .get("kind")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            let issuer = CertifiedIssuer::self_signed(
                CertificateParams::default(),
                KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap(),
            )
            .unwrap();
            let certificate = CertificateSigningRequestParams::from_pem(
                credential
                    .get("spec")
                    .unwrap()
                    .get("csr")
                    .unwrap()
                    .as_str()
                    .unwrap(),
            )
            .unwrap()
            .signed_by(&issuer)
            .unwrap();
            let certificate: Certificate = certificate.pem().parse().unwrap();
            requests.issued_certificate = Some(certificate.encode_der().unwrap());
            credential.as_object_mut().unwrap().insert(
                "status".to_owned(),
                json!({"certificate": certificate.encode_pem().unwrap()}),
            );
            (StatusCode::CREATED, Json(credential))
        }
        (Method::GET, TARGET_PATH) => {
            let ci_info = url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
                .find_map(|(name, value)| {
                    (name == "session_ci_info").then(|| serde_json::from_str(&value).unwrap())
                })
                .unwrap_or(Value::Null);
            let certificate = headers
                .get("x-client-der")
                .map(|value| BASE64_STANDARD.decode(value.as_bytes()).unwrap())
                .unwrap_or_default();
            requests.connections.push((ci_info, certificate));

            // Admission rejection ends the real connection path before proxies or user processes
            // are started, so these credential tests need neither a cluster nor a container image.
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "apiVersion": "v1",
                    "kind": "Status",
                    "status": "Failure",
                    "reason": "Forbidden",
                    "message": ADMISSION_ERROR,
                    "code": 403
                })),
            )
        }
        (method, path) => {
            requests.unexpected.push(format!("{method} {path}"));
            (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "unexpected fixture request", "code": 404})),
            )
        }
    }
}

struct Fixture {
    directory: TempDir,
    requests: Arc<Mutex<Requests>>,
    server: JoinHandle<()>,
}

impl Fixture {
    async fn new(feature_fields: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut operator = json!({
            "apiVersion": "operator.metalbear.co/v1",
            "kind": "MirrordOperator",
            "metadata": {"name": "operator"},
            "spec": {
                "operator_version": env!("CARGO_PKG_VERSION"),
                "default_namespace": "default",
                "license": {
                    "name": "Free",
                    "organization": "CI credential fixture",
                    "expire_at": "4096-01-01",
                    "fingerprint": "Y29yLTE3MzE="
                }
            }
        });
        operator
            .get_mut("spec")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .extend(feature_fields.as_object().unwrap().clone());
        let requests = Arc::new(Mutex::new(Requests {
            operator,
            ..Default::default()
        }));
        let app = Router::new()
            .route("/{*path}", any(operator_request))
            .with_state(requests.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // A separate home and temp directory keep credential caching and CI process state local
        // to this CLI invocation without changing the test runner's environment.
        std::fs::create_dir(directory.path().join("tmp")).unwrap();
        std::fs::write(
            directory.path().join("kubeconfig.json"),
            serde_json::to_vec(&json!({
                "apiVersion": "v1",
                "kind": "Config",
                "clusters": [{"name": "fixture", "cluster": {"server": format!("http://{address}")}}],
                "users": [{"name": "fixture", "user": {}}],
                "contexts": [{"name": "fixture", "context": {"cluster": "fixture", "user": "fixture", "namespace": "default"}}],
                "current-context": "fixture"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("mirrord.toml"),
            "operator = true\ntelemetry = false\nkubeconfig = 'kubeconfig.json'\n\
             [startup_retry]\nmax_retries = 0\n",
        )
        .unwrap();

        Self {
            directory,
            requests,
            server,
        }
    }

    async fn run_ci(&self, subcommand: &str, api_key: Option<&str>) -> Output {
        let mut command = resolve_tokio_command(env!("CARGO_BIN_EXE_mirrord"));
        command
            .current_dir(self.directory.path())
            .env_clear()
            .env("HOME", self.directory.path())
            .env("TMPDIR", self.directory.path().join("tmp"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("MIRRORD_PROGRESS_MODE", "off")
            .args([
                "ci",
                subcommand,
                "--foreground",
                "--environment",
                "staging",
                "--pipeline",
                "credential-regression",
                "--triggered-by",
                "manual",
                "--config-file",
                "mirrord.toml",
                "--no-telemetry",
                "--disable-version-check",
                "--",
            ])
            .kill_on_drop(true);
        if subcommand == "container" {
            command.args(["podman", "run", "unused-fixture-image"]);
        } else {
            command.arg("/bin/true");
        }
        if let Some(api_key) = api_key {
            command.env("MIRRORD_CI_API_KEY", api_key);
        }
        timeout(Duration::from_secs(30), command.output())
            .await
            .expect("CI credential selection should not hang")
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[rstest]
#[case::automatic_credentials(false, false)]
#[case::expired_ordinary_credentials(false, true)]
#[case::provided_ci_key_without_capability(true, false)]
#[tokio::test]
async fn ci_commands_preserve_credentials_and_metadata(
    #[case] with_key: bool,
    #[case] cached_expired: bool,
) {
    let supported_features = if with_key {
        vec!["ExtendableUserCredentials"]
    } else {
        vec!["ExtendableUserCredentials", "KeylessCi"]
    };
    let fixture = Fixture::new(json!({"supported_features": supported_features})).await;
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let mut certificate_params = CertificateParams::default();
    if cached_expired {
        certificate_params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        certificate_params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let certificate = certificate_params.self_signed(&key_pair).unwrap();
    let credentials: Credentials = serde_json::from_value(json!({
        "certificate": certificate.pem(),
        "key_pair": key_pair.serialize_pem()
    }))
    .unwrap();
    if cached_expired {
        let credentials_dir = fixture.directory.path().join(".mirrord");
        std::fs::create_dir(&credentials_dir).unwrap();
        std::fs::write(
            credentials_dir.join("credentials"),
            serde_json::to_vec(&json!({
                "credentials": {"Y29yLTE3MzE=": &credentials},
                "signing_keys": {}
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let supplied_certificate = credentials.as_ref().encode_der().unwrap();
    let encoded_key = CiApiKey::V1(credentials)
        .encode_as_url_safe_string()
        .unwrap();

    // The second command must reuse the ordinary cached credential, or keep using the supplied
    // key, while preserving explicit metadata in both command handlers.
    for subcommand in ["start", "container"] {
        let output = fixture
            .run_ci(subcommand, with_key.then_some(encoded_key.as_str()))
            .await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success().not());
        assert!(stderr.contains(ADMISSION_ERROR), "{subcommand}: {stderr}");
    }

    let requests = fixture.requests.lock().await;
    assert!(requests.unexpected.is_empty(), "{:?}", requests.unexpected);
    assert_eq!(requests.operator_reads, 2);
    assert_eq!(requests.connections.len(), 2);
    let expected_certificate = if with_key {
        assert!(requests.credential_kinds.is_empty());
        assert!(
            fixture
                .directory
                .path()
                .join(".mirrord/credentials")
                .exists()
                .not()
        );
        supplied_certificate.as_slice()
    } else {
        assert_eq!(requests.credential_kinds, ["regular"]);
        requests.issued_certificate.as_deref().unwrap()
    };
    for (ci_info, sent_certificate) in &requests.connections {
        assert_eq!(ci_info["environment"], "staging");
        assert_eq!(ci_info["pipeline"], "credential-regression");
        assert_eq!(ci_info["triggeredBy"], "manual");
        assert_eq!(sent_certificate, expected_certificate);
    }
}

#[rstest]
#[case::capability_absent(json!({"supported_features": ["ExtendableUserCredentials"]}))]
#[case::legacy_discovery(json!({"features": ["ProxyApi"], "copy_target_enabled": true}))]
#[case::feature_fields_omitted(json!({}))]
#[tokio::test]
async fn missing_ci_key_requires_advertised_capability(#[case] feature_fields: Value) {
    let fixture = Fixture::new(feature_fields).await;
    for subcommand in ["start", "container"] {
        let output = fixture.run_ci(subcommand, None).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Miette's wrapping and continuation gutters must not affect diagnostic assertions.
        let diagnostic = stderr
            .split_whitespace()
            .filter(|word| *word != "│")
            .collect::<Vec<_>>()
            .join(" ");
        assert!(output.status.success().not());
        assert!(
            diagnostic.contains("`MIRRORD_CI_API_KEY` is required"),
            "{subcommand}: {stderr}"
        );
        assert!(
            diagnostic.contains("does not advertise keyless CI"),
            "{subcommand}: {stderr}"
        );
    }

    let requests = fixture.requests.lock().await;
    assert_eq!(requests.operator_reads, 2);
    assert!(requests.credential_kinds.is_empty());
    assert!(requests.connections.is_empty());
    assert!(requests.unexpected.is_empty(), "{:?}", requests.unexpected);
    assert!(
        fixture
            .directory
            .path()
            .join(".mirrord/credentials")
            .exists()
            .not()
    );
}

#[rstest]
#[case::malformed("malformed-ci-key")]
#[case::empty("")]
#[tokio::test]
async fn malformed_ci_key_does_not_fall_back_to_ordinary_credentials(#[case] api_key: &str) {
    let fixture = Fixture::new(json!({
        "supported_features": ["ExtendableUserCredentials", "KeylessCi"]
    }))
    .await;
    for subcommand in ["start", "container"] {
        let output = fixture.run_ci(subcommand, Some(api_key)).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success().not());
        assert!(
            stderr.contains("invalid api key format"),
            "{subcommand}: {stderr}"
        );
    }

    let requests = fixture.requests.lock().await;
    assert_eq!(requests.operator_reads, 0);
    assert!(requests.credential_kinds.is_empty());
    assert!(requests.connections.is_empty());
    assert!(requests.unexpected.is_empty());
}
