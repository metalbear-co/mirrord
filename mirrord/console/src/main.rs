mod protocol;

use std::any::type_name;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use protocol::Hello;
use tracing::{error, info, metadata::LevelFilter};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

struct WebSocketWrapper {
    socket: WebSocket,
}

impl WebSocketWrapper {
    fn new(socket: WebSocket) -> Self {
        Self { socket }
    }

    async fn next_message<T>(&mut self) -> Option<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let raw_msg = self.socket.recv().await;
        let msg = match raw_msg {
            None => {
                error!("Client disconnected");
                return None;
            }
            Some(Ok(Message::Binary(msg))) => msg,
            _ => {
                error!("Invalid message {raw_msg:?}");
                return None;
            }
        };

        match serde_json::from_slice::<T>(&msg) {
            Ok(record) => Some(record),
            Err(e) => {
                let type_name = type_name::<T>();
                error!("Client message error: {e:?}, was expecting {type_name:?}");
                None
            }
        }
    }
}

async fn handle_socket(socket: WebSocket) {
    let mut wrapper = WebSocketWrapper::new(socket);

    let client_info: Hello = match wrapper.next_message().await {
        Some(hello) => {
            info!("Client connected -  process info {hello:?}");
            hello
        }
        None => {
            error!("Client disconnected");
            return;
        }
    };

    while let Some(record) = wrapper.next_message::<protocol::Record>().await {
        let logger = log::logger();
        logger.log(
            &log::Record::builder()
                .args(format_args!(
                    "pid {:?}: {:?}",
                    client_info.process_info.id, record.message
                ))
                .file(record.file.as_deref())
                .level(record.metadata.level)
                .module_path(record.module_path.as_deref())
                .target(&record.metadata.target)
                .line(record.line)
                .build(),
        );
    }
    info!("Client disconnected pid: {:?}", client_info.process_info.id)
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
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
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
