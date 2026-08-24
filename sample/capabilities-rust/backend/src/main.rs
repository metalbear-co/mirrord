use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::Query,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use tower_http::{
    LatencyUnit,
    cors::{Any, CorsLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

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
struct OutgoingQuery {
    url: String,
}

#[derive(Debug, Serialize)]
struct OutgoingResponse {
    url: String,
    status: u16,
    body: String,
    body_preview: String,
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

    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/meta", get(meta))
        .route("/env", get(env_dump))
        .route("/outgoing", get(outgoing));

    let trace_layer = TraceLayer::new_for_http()
        .on_request(|request: &Request<_>, _span: &tracing::Span| {
            tracing::debug!(
                method = %request.method(),
                path = %request.uri().path(),
                headers = ?request.headers(),
                "started request"
            )
        })
        .on_response(
            DefaultOnResponse::new()
                .level(Level::INFO)
                .latency_unit(LatencyUnit::Millis),
        );

    let app = Router::new()
        .merge(api.clone())
        .nest("/demo/api", api)
        .layer(cors)
        .layer(trace_layer)
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
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) => format!("failed to read body: {error}"),
            };
            let body_preview = body.chars().take(200).collect::<String>();
            (
                StatusCode::OK,
                Json(OutgoingResponse {
                    url: query.url,
                    status,
                    body,
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
