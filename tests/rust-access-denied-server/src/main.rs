//! Access Denied as a Service

use axum::{
    Router,
    http::{Method, StatusCode, Uri},
    routing::any,
};

async fn handler(method: Method, uri: Uri) -> StatusCode {
    println!("403 {method} {uri}");
    StatusCode::FORBIDDEN
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let app = Router::new().route("/{*path}", any(handler));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:4566").await.unwrap();
    eprintln!("access-denied-service ready");
    axum::serve(listener, app).await.unwrap();
}
