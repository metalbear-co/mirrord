//! Test app that opens an HTTP server at port `80`. It accepts WebSockets connections on the root
//! path and echoes all [`Message::Text`] and [`Message::Binary`] messages. The app exists
//! successfully after any WebSockets connection closes.

use std::net::SocketAddr;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    routing::get,
    Router,
};
use tokio::sync::mpsc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let addr: SocketAddr = "0.0.0.0:80".parse().unwrap();

    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel::<()>();

    let app = Router::new()
        .route(
            "/",
            get(
                |ws: WebSocketUpgrade, shutdown: State<mpsc::UnboundedSender<()>>| async move {
                    ws.on_upgrade(move |socket| handle_socket(socket, shutdown.0))
                },
            ),
        )
        .with_state(shutdown_tx);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(async move {
            shutdown_rx.recv().await;
        })
        .await
        .unwrap();
}

async fn handle_socket(mut socket: WebSocket, shutdown: mpsc::UnboundedSender<()>) {
    loop {
        let msg = match socket.recv().await {
            Some(Ok(msg)) => msg,
            Some(Err(err)) => {
                eprintln!("Error while receiving message: {err:?}");
                break;
            }
            None => {
                eprintln!("Connection broke");
                break;
            }
        };

        let res = match msg {
            Message::Binary(data) => socket.send(Message::Binary(data)).await,
            Message::Text(text) => socket.send(Message::Text(text)).await,
            Message::Close(close_frame) => {
                eprintln!(
                    "Client closed connection with reason: {:?}",
                    close_frame.map(|frame| frame.reason)
                );
                shutdown.send(()).ok();
                break;
            }
            Message::Ping(..) | Message::Pong(..) => {
                // ping pong is handled by axum framework
                Ok(())
            }
        };

        if let Err(err) = res {
            eprintln!("Error while sending message: {err:?}");
            break;
        }
    }
}
