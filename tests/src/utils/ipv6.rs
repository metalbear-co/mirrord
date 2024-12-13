use http_body_util::{BodyExt, Empty};
use hyper::{
    client::{conn, conn::http1::SendRequest},
    Request,
};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use rstest::fixture;

use crate::utils::{internal_service, kube_client, KubeService};

/// Create a new [`KubeService`] and related Kubernetes resources. The resources will be deleted
/// when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see
/// [`PRESERVE_FAILED_ENV_NAME`](crate::utils::PRESERVE_FAILED_ENV_NAME).
///
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
#[fixture]
pub async fn ipv6_service(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    // #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("docker.io/t4lz/mirrord-pytest:1204")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client.await,
        serde_json::json!([
            {
              "name": "HOST",
              "value": "::"
            }
        ]),
        true,
    )
    .await
}

/// Send an HTTP request using the referenced `request_sender`, with the provided `method`,
/// then verify a success status code, and a response body that is the used method.
///
/// # Panics
/// - If the request cannot be sent.
/// - If the response's code is not OK
/// - If the response's body is not the method's name.
pub async fn send_request_with_method(
    method: &str,
    request_sender: &mut SendRequest<Empty<hyper::body::Bytes>>,
) {
    let req = Request::builder()
        .method(method)
        .body(Empty::<hyper::body::Bytes>::new())
        .unwrap();

    let res = request_sender.send_request(req).await.unwrap();
    println!("Response: {:?}", res);
    assert_eq!(res.status(), hyper::StatusCode::OK);
    let bytes = res.collect().await.unwrap().to_bytes();
    let response_string = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(response_string, method);
}

/// Create a portforward to the pod of the test service, and send HTTP requests over it.
/// Send four HTTP request (GET, POST, PUT, DELETE), using the referenced `request_sender`, with the
/// provided `method`, verify OK status, and a response body that is the used method.
///
/// # Panics
/// - If a request cannot be sent.
/// - If a response's code is not OK
/// - If a response's body is not the method's name.
pub async fn portforward_http_requests(api: &Api<Pod>, service: KubeService) {
    let mut portforwarder = api
        .portforward(&service.pod_name, &[80])
        .await
        .expect("Failed to start portforward to test pod");

    let stream = portforwarder.take_stream(80).unwrap();
    let stream = hyper_util::rt::TokioIo::new(stream);

    let (mut request_sender, connection) = conn::http1::handshake(stream).await.unwrap();
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("Error in connection from test function to deployed test app {err:#?}");
        }
    });
    for method in ["GET", "POST", "PUT", "DELETE"] {
        send_request_with_method(method, &mut request_sender).await;
    }
}
