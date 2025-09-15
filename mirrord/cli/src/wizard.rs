use std::fs::create_dir_all;
use std::io::Cursor;
use std::net::SocketAddr;
use std::path::Path;
use axum::Router;
use tokio::process::Command;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tar::Archive;
use flate2::read::GzDecoder;

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

pub async fn wizard_command() {
    // TODO: ascii <|:)
    // TODO: serve wizard <|:)

    // unpack frontend into dir
    let temp_dir_path = Path::new("/tmp/mirrord-wizard-assets");
    create_dir_all(temp_dir_path).unwrap();

    let tar_gz = Cursor::new(COMPRESSED_FRONTEND);
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(temp_dir_path).unwrap();

    // prepare temp dir as SPA
    let app = Router::new().fallback_service(ServeDir::new(temp_dir_path));
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());

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

    // TODO: join the serve and window opening (to prevent race condition)
    // serve
    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .await
        .unwrap();

    // TODO: on drop, cleanup files (?)
}
