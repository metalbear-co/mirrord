//! Local counterpart of the [http-keep-alive]() server.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_default_env()
        .format_timestamp_secs()
        .write_style(env_logger::WriteStyle::Never)
        .init();

    HttpServer::new(|| App::new().service(index).wrap(Logger::default()))
        .bind(("0.0.0.0", 80))
        .map(|server| {
            info!("Listener for issue1317: STARTED");
            server
        })?
        .keep_alive(Duration::from_secs(240))
        .run()
        .await
}

#[get("/")]
#[tracing::instrument(level = "info", ret)]
async fn index(incoming: String) -> String {
    println!("Echo [local]: {incoming}");
    format!("Echo [local]: {incoming}")
}

use std::time::Duration;

use actix_web::{get, middleware::Logger, App, HttpServer};
use tracing::info;
