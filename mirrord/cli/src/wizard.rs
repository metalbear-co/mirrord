use std::{io::Cursor, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::{Path, Query, State},
    http::header,
    routing::get,
};
use flate2::read::GzDecoder;
use mirrord_config::target::TargetType;
use serde::{Deserialize, Serialize};
use tar::Archive;
use tempfile::TempDir;
use tokio::{process::Command, sync::Mutex};
use tower::ServiceBuilder;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::{error::CliResult, user_data::UserData};

const COMPRESSED_FRONTEND: &[u8] = include_bytes!("../../../wizard-frontend.tar.gz");
#[cfg(target_os = "linux")]
fn get_open_command() -> Command {
    let mut command = Command::new("gio");
    command.arg("open");
    command
}

#[cfg(target_os = "macos")]
fn get_open_command() -> Command {
    Command::new("open")
}

/// WARNING: untested on `target_os = "windows"`, but if this fails the URL will get printed to the
/// terminal as a fallback.
#[cfg(target_os = "windows")]
fn get_open_command() -> Command {
    Command::new("cmd.exe").arg("/C").arg("start").arg("")
}

pub async fn wizard_command(user_data: UserData) {
    println!("<|:) Greetings, traveler!");
    println!("Opening the Wizard in the browser...");

    let is_returning_string = user_data.is_returning_wizard().to_string();

    // unpack frontend into dir
    let tmp_dir = TempDir::new().unwrap(); // FIXME: remove unwraps
    let temp_dir_path = tmp_dir.path();

    let tar_gz = Cursor::new(COMPRESSED_FRONTEND);
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(temp_dir_path).unwrap(); // FIXME: remove unwraps

    let serve_client = {
        let index_service = ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                header::PRAGMA,
                header::HeaderValue::from_static("no-cache"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                header::EXPIRES,
                header::HeaderValue::from_static("0"),
            ))
            .service(ServeFile::new(temp_dir_path.join("index.html")));

        ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::if_not_present(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("public, max-age=1209600"),
            ))
            .service(
                ServeDir::new(temp_dir_path)
                    .fallback(index_service)
                    .append_index_html_on_directories(false),
            )
    };

    // prepare temp dir as SPA with endpoints behind `/api` path
    let app = Router::new()
        .fallback_service(serve_client)
        .route(
            "/api/v1/is-returning",
            get(|| async { is_returning_string }),
        )
        .route("/api/v1/cluster-details", get(cluster_details))
        .route("/api/v1/namespace/{namespace}/targets", get(list_targets))
        .with_state(Arc::new(Mutex::new(user_data)));
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap(); // FIXME: remove unwraps
    tracing::debug!("listening on {}", listener.local_addr().unwrap()); // FIXME: remove unwraps

    // open browser window
    let url = "http://localhost:3000/";
    match crate::wizard::get_open_command().arg(&url).output().await {
        Ok(output) if output.status.success() => {}
        other => {
            tracing::trace!(?other, "failed to open browser");
            println!(
                "To open the mirrord wizard, visit:\n\n\
             {url}"
            );
        }
    }

    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .await
        .unwrap(); // FIXME: remove unwraps
}

#[derive(Debug, Serialize)]
struct ClusterDetails {
    namespaces: Vec<String>,
    target_types: Vec<TargetType>,
}

#[derive(Debug, Serialize)]
struct TargetInfo {
    target_path: String,
    target_namespace: String,
    detected_ports: Vec<u16>,
}

#[derive(Debug, Deserialize)]
struct Params {
    target_type: Option<TargetType>,
}

async fn cluster_details(State(user_data): State<Arc<Mutex<UserData>>>) -> CliResult<String> {
    let mut user_guard = user_data.lock().await;
    // consider the user a returning user in future runs
    if !user_guard.is_returning_wizard() {
        // ignore failures to update
        let _ = user_guard.update_is_returning_wizard().await;
    }

    // TODO: return available namespaces and target types

    let res = ClusterDetails {
        namespaces: vec![
            "default".to_string(),
            "production".to_string(),
            "system".to_string(),
            "kube-system".to_string(),
            "monitoring".to_string(),
        ],
        target_types: TargetType::all().collect(),
    };
    Ok(serde_json::to_string(&res)?)
}

async fn list_targets(
    Query(query_params): Query<Params>,
    Path(namespace): Path<String>,
) -> CliResult<String> {
    // TODO: return target info using mirrord ls

    let targets = vec![
        TargetInfo {
            target_path: "deployment/api-service".to_string(),
            target_namespace: "default".to_string(),
            detected_ports: vec![8080, 3000, 5432, 9000, 4000],
        },
        TargetInfo {
            target_path: "deployment/worker-queue".to_string(),
            target_namespace: "production".to_string(),
            detected_ports: vec![8080, 3000, 4000, 6379, 5672, 3306],
        },
        TargetInfo {
            target_path: "cronjob/backup-job".to_string(),
            target_namespace: "system".to_string(),
            detected_ports: vec![3000, 5432, 9000, 4000, 6379, 5672],
        },
    ];

    let thing = query_params.target_type;
    println!("{:?}", thing);

    let res = targets
        .iter()
        .filter(|target| target.target_namespace == namespace)
        .collect::<Vec<_>>();
    Ok(serde_json::to_string(&res)?)
}
