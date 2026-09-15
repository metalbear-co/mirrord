//! Local counterpart of the
//! [http-keep-alive](https://github.com/metalbear-co/test-images/tree/main/http-keep-alive) server.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_default_env()
        .format_timestamp_secs()
        .write_style(env_logger::WriteStyle::Never)
        .init();

    // HTTP/1.1 keep-alive is on by default and connections are held until the peer closes
    // them, which is what this server exists to exercise.
    let listener = TcpListener::bind(("0.0.0.0", 80)).await?;
    info!("Listener for issue1317: STARTED");

    axum::serve(listener, Router::new().route("/", get(index))).await
}

#[tracing::instrument(level = "info", ret)]
async fn index(incoming: String) -> String {
    // If the body contains `EXIT`, then we quit this process.
    if incoming.contains("EXIT") {
        eprintln!("Exiting process!");
        std::process::exit(0);
    } else {
        eprintln!("Echo [local]: {incoming}");
        format!("Echo [local]: {incoming}")
    }
}

use axum::{Router, routing::get};
use tokio::net::TcpListener;
use tracing::info;
