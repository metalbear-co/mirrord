use std::{io::Cursor, net::SocketAddr, time::Duration};

use axum::{Router, http::header, Json};
use axum::extract::Path;
use axum::routing::get;
use flate2::read::GzDecoder;
use tar::Archive;
use tempfile::TempDir;
use tokio::{process::Command, time::sleep};
use tower::ServiceBuilder;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::user_data::UserData;

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

pub async fn wizard_command(user_data: &mut UserData) {
    // TODO: ascii <|:) printout
    let is_returning = user_data.is_returning_wizard();

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
        .route("/api/v1/{key}", get(api_handler));
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

    // when 10 seconds elapses, update user data from new to returning
    if !is_returning {
        tokio::spawn(async {
            sleep(Duration::from_secs(10)).await;
            let _ = user_data.update_is_returning_wizard().await;
        });
    }

    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .await
        .unwrap(); // FIXME: remove unwraps
}

struct TargetInfo {
    target_path: String,
}
enum WizardData {
    IsReturning(bool),
    ClusterData(Vec<TargetInfo>),
}

async fn api_handler(Path(path): Path<String>) -> Json<WizardData> {
    match path.as_str() {
        "get-returning" => WizardData::IsReturning(true).into(),
        "get-cluster-data" => (),
        _ => () // unknown api endpoint, ignore
    }
}