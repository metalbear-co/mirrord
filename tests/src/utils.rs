use std::{collections::HashMap, env, panic, path::Path, process, process::Stdio};

use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Namespace, Pod, Service},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    api::{DeleteParams, ListParams, PostParams},
    Api, Client, Config,
};
use reqwest::{Method, StatusCode};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdout, Command},
    time::{sleep, Duration},
};

static TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

// target/debug/mirrord exec --pod-name pod_name  -c binary command
pub fn start_node_server(pod_name: &str, command: Vec<&str>, env: HashMap<&str, &str>) -> Child {
    let path = env!("CARGO_BIN_FILE_MIRRORD");
    let args: Vec<&str> = vec!["exec", "--pod-name", pod_name, "-c"]
        .into_iter()
        .chain(command.into_iter())
        .collect();
    let server = Command::new(path)
        .args(args)
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    server
}

pub async fn setup_kube_client() -> Client {
    let mut config = Config::infer().await.unwrap();
    config.accept_invalid_certs = true;
    let client = Client::try_from(config).unwrap();
    client
}

// minikube service nginx --url
pub async fn get_service_url(client: &Client, namespace: &str) -> Option<String> {
    let service_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let services = service_api
        .list(&ListParams::default().labels("app=nginx"))
        .await
        .unwrap();
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pods = pod_api
        .list(&ListParams::default().labels("app=nginx"))
        .await
        .unwrap();
    let host_ip = pods
        .into_iter()
        .next()
        .and_then(|pod| pod.status)
        .and_then(|status| status.host_ip.clone());
    let port = services
        .into_iter()
        .next()
        .and_then(|service| service.spec)
        .and_then(|spec| spec.ports.clone())
        .and_then(|mut ports| ports.pop());
    Some(format!(
        "http://{}:{}",
        host_ip.unwrap(),
        port.unwrap().node_port.unwrap()
    ))
}

// kubectl get pods | grep nginx
pub async fn get_nginx_pod_name(client: &Client, namespace: &str) -> Option<String> {
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pods = pod_api
        .list(&ListParams::default().labels("app=nginx"))
        .await
        .unwrap();
    let pod = pods
        .iter()
        .next()
        .map(|pod| pod.metadata.name.clone())
        .flatten();
    pod
}

// kubectl create namespace name
pub async fn create_namespace(client: &Client, namespace: &str) {
    let namespace_api: Api<Namespace> = Api::all(client.clone());
    let new_namespace = Namespace {
        metadata: ObjectMeta {
            name: Some(namespace.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    namespace_api
        .create(&PostParams::default(), &new_namespace)
        .await
        .unwrap();
}

// kubectl delete namespace name
pub async fn delete_namespace(client: &Client, namespace: &str) {
    let namespace_api: Api<Namespace> = Api::all(client.clone());
    namespace_api
        .delete(namespace, &DeleteParams::default())
        .await
        .unwrap();
}

pub async fn http_request(url: &str, method: Method) {
    let client = reqwest::Client::new();
    let res = client
        .request(method.clone(), url)
        .body(TEXT)
        .send()
        .await
        .unwrap();
    match method {
        Method::GET => assert_eq!(res.status(), StatusCode::OK),
        Method::POST | Method::PUT | Method::DELETE => {
            assert_eq!(res.status(), StatusCode::from_u16(405).unwrap())
        }
        _ => panic!("unexpected method"),
    }
}

// kubectl apply -f tests/app.yaml -n name
pub async fn create_nginx_pod(client: &Client, namespace: &str) {
    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": "nginx",
            "labels": {
                "app": "nginx"
            }
        },
        "spec": {
            "replicas": 1,
            "selector": {
                "matchLabels": {
                    "app": "nginx"
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "app": "nginx"
                    }
                },
                "spec": {
                    "containers": [
                        {
                            "name": "nginx",
                            "image": "nginx:1.14.2",
                            "ports": [
                                {
                                    "containerPort": 80
                                }
                            ]
                        }
                    ]
                }
            }
        }
    }))
    .unwrap();

    deployment_api
        .create(&PostParams::default(), &deployment)
        .await
        .unwrap();

    let service_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let service = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": "nginx",
            "labels": {
                "app": "nginx"
            }
        },
        "spec": {
            "type": "NodePort",
            "selector": {
                "app": "nginx"
            },
            "sessionAffinity": "None",
            "ports": [
                {
                    "protocol": "TCP",
                    "port": 80,
                    "targetPort": 80
                }
            ]
        }
    }))
    .unwrap();

    service_api
        .create(&PostParams::default(), &service)
        .await
        .unwrap();
}

// to all requests the express API prints {request_name}: Request completed
// PUT - creates /tmp/test, DELETE - deletes /tmp/test
pub async fn validate_requests(stdout: ChildStdout, service_url: &str) {
    let mut buffer = BufReader::new(stdout);
    let mut stream = String::new();
    buffer.read_line(&mut stream).await.unwrap();
    assert!(stream.contains("Server listening on port 80"));

    http_request(service_url, Method::GET).await;
    buffer.read_line(&mut stream).await.unwrap();
    assert!(stream.contains("GET: Request completed"));

    http_request(service_url, Method::POST).await;
    buffer.read_line(&mut stream).await.unwrap();
    assert!(stream.contains("POST: Request completed"));

    http_request(service_url, Method::PUT).await;
    sleep(Duration::from_secs(2)).await;
    assert!(Path::new("/tmp/test").exists());
    buffer.read_line(&mut stream).await.unwrap();
    assert!(stream.contains("PUT: Request completed"));

    http_request(service_url, Method::DELETE).await;
    sleep(Duration::from_secs(2)).await;
    assert!(!Path::new("/tmp/test").exists());
    buffer.read_line(&mut stream).await.unwrap();
    assert!(stream.contains("DELETE: Request completed"));
}

pub async fn validate_no_requests(stdout: ChildStdout, service_url: &str) {
    let mut buffer = BufReader::new(stdout);
    let mut stream = String::new();
    buffer.read_line(&mut stream).await.unwrap();
    assert!(stream.contains("Server listening on port 80"));
    http_request(service_url, Method::PUT).await;
    sleep(Duration::from_secs(5)).await;
    assert!(!Path::new("/tmp/test").exists()); // the API creates a file in /tmp/, which should not
                                               // exist
}

// initializes the test/runs the node process
pub async fn test_server_init(
    client: &Client,
    pod_namespace: &str,
    mut env: HashMap<&str, &str>,
) -> Child {
    let pod_name = get_nginx_pod_name(&client, pod_namespace).await.unwrap();
    let command = vec!["node", "node-e2e/app.js"];
    // used by the CI, to load the image locally:
    // docker build -t test . -f mirrord-agent/Dockerfile
    // minikube load image test:latest
    env.insert("MIRRORD_AGENT_IMAGE", "test");
    let server = start_node_server(&pod_name, command, env);
    setup_panic_hook();
    server
}

// can't panic from a task, this utility just exits the process with an error code
pub fn setup_panic_hook() {
    let orig_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        orig_hook(panic_info);
        process::exit(1);
    }));
}
