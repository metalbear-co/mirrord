mod protocol;

use axum::{
    extract::ws::{WebSocket, WebSocketUpgrade, Message},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use tracing::{metadata::LevelFilter, info};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};
use protocol::Hello;

/// Receives the first message in the web socket connection.
/// If the message is valid, it will handle it and return True
/// If the message is invalid, it will return false
async fn handle_hello(mut socket: &WebSocket) -> bool {
    if let Some(Ok(Message::Binary(msg))) = socket.recv().await {
        if let Ok(msg) = serde_json::from_slice::<Hello>(&msg) {
            info!("Client with process info {msg:?}");
        }
    }
    false
}
async fn handle_socket(mut socket: WebSocket) {

    while let Some(msg) = socket.recv().await {
        let msg = if let Ok(msg) = msg {
            msg
        } else {
            // client disconnected
            return;
        };

        if socket.send(msg).await.is_err() {
            // client disconnected
            return;
        }
    }
}

async fn handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_socket)
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/ws", get(handler));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_span_events(FmtSpan::ACTIVE)
                .compact(),
        )
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::TRACE.into())
                .from_env_lossy(),
        )
        .init();

    // run it with hyper on localhost:11233
    axum::Server::bind(&"0.0.0.0:11233".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
