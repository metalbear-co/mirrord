use std::{
    collections::HashMap,
    env,
    net::{SocketAddr, ToSocketAddrs},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    startup_unix_secs: u64,
    outgoing_timeout_ms: u64,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct MetaResponse {
    hostname: String,
    pid: u32,
    startup_unix_secs: u64,
}

#[derive(Debug, Serialize)]
struct EnvResponse {
    values: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct DnsQuery {
    host: String,
}

#[derive(Debug, Serialize)]
struct DnsResponse {
    host: String,
    addrs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OutgoingQuery {
    url: String,
}

#[derive(Debug, Serialize)]
struct OutgoingResponse {
    url: String,
    status: u16,
    body_preview: String,
}

#[derive(Debug, Serialize)]
struct EchoResponse {
    headers: HashMap<String, String>,
    body: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let bind_host = env::var("DEMO_BIND_ADDR")
        .ok()
        .unwrap_or_else(|| "0.0.0.0".parse().expect("static addr"));
    let bind_port = env::var("DEMO_BIND_PORT")
        .ok()
        .unwrap_or_else(|| "8080".parse().expect("static port"));
    let bind_addr = format!("{bind_host}:{bind_port}")
        .parse::<SocketAddr>()
        .expect("static addr");

    let outgoing_timeout_ms = env::var("DEMO_OUTGOING_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(2_000);
    let startup_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let state = AppState {
        startup_unix_secs,
        outgoing_timeout_ms,
    };
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/meta", get(meta))
        .route("/env", get(env_dump))
        .route("/dns", get(dns_lookup))
        .route("/outgoing", get(outgoing))
        .route("/echo", post(echo))
        .layer(cors)
        .with_state(state);

    tracing::info!("capabilities-rust backend listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .expect("bind should succeed");
    axum::serve(listener, app).await.expect("server should run");
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn meta(axum::extract::State(state): axum::extract::State<AppState>) -> Json<MetaResponse> {
    let hostname = env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned());
    Json(MetaResponse {
        hostname,
        pid: process::id(),
        startup_unix_secs: state.startup_unix_secs,
    })
}

async fn env_dump(_: axum::extract::State<AppState>) -> Json<EnvResponse> {
    let values = env::vars().collect::<HashMap<_, _>>();
    Json(EnvResponse { values })
}

async fn dns_lookup(Query(query): Query<DnsQuery>) -> impl IntoResponse {
    match (query.host.as_str(), 0u16).to_socket_addrs() {
        Ok(addrs) => {
            let addrs = addrs.map(|addr| addr.ip().to_string()).collect::<Vec<_>>();
            (
                StatusCode::OK,
                Json(DnsResponse {
                    host: query.host,
                    addrs,
                }),
            )
                .into_response()
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            format!("dns lookup failed: {error}"),
        )
            .into_response(),
    }
}

async fn outgoing(
    axum::extract::State(state): axum::extract::State<AppState>,
    Query(query): Query<OutgoingQuery>,
) -> impl IntoResponse {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(state.outgoing_timeout_ms))
        .build()
        .expect("client should build");

    match client.get(&query.url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body_preview = match response.text().await {
                Ok(body) => body.chars().take(200).collect::<String>(),
                Err(error) => format!("failed to read body: {error}"),
            };
            (
                StatusCode::OK,
                Json(OutgoingResponse {
                    url: query.url,
                    status,
                    body_preview,
                }),
            )
                .into_response()
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("outgoing request failed: {error}"),
        )
            .into_response(),
    }
}

async fn echo(headers: HeaderMap, body: String) -> Json<EchoResponse> {
    let headers = headers
        .iter()
        .map(|(name, value)| {
            let value = value
                .to_str()
                .map(|raw| raw.to_owned())
                .unwrap_or_else(|_| "<non-utf8>".to_owned());
            (name.to_string(), value)
        })
        .collect::<HashMap<_, _>>();

    Json(EchoResponse { headers, body })
}
