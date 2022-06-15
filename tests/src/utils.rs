use std::{collections::HashMap, env, fmt::Debug, panic, process, process::Stdio};

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Namespace, Pod, Service},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    api::{DeleteParams, ListParams, PostParams},
    core::WatchEvent,
    Api, Client, Config,
};
use lazy_static::lazy_static;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{Child, ChildStdout, Command},
};

static TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

macro_rules! assert_contains {
    ($hay: ident, $needle: expr) => {
        assert!(
            $hay.contains($needle),
            "{:?} not found in stream: {:?}",
            $needle,
            $hay
        )
    };
}

lazy_static! {
    static ref SERVERS: HashMap<&'static str, Vec<&'static str>> = HashMap::from([
        ("python", vec!["python3", "-u", "python-e2e/app.py"]),
        ("node", vec!["node", "node-e2e/app.js"])
    ]);
}

// target/debug/mirrord exec --pod-name pod_name  -c binary command
pub fn start_server(pod_name: &str, command: Vec<&str>, env: HashMap<&str, &str>) -> Child {
    let path = env!("CARGO_BIN_FILE_MIRRORD");
    println!(">>>>> path: {}", path);
    let args: Vec<&str> = vec!["exec", "--pod-name", pod_name, "-c", "--"]
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
    Client::try_from(config).unwrap()
}

// minikube service http-echo --url
pub async fn get_service_url(client: &Client, namespace: &str) -> Option<String> {
    let service_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let services = service_api
        .list(&ListParams::default().labels("app=http-echo"))
        .await
        .unwrap();
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pods = pod_api
        .list(&ListParams::default().labels("app=http-echo"))
        .await
        .unwrap();
    let host_ip = pods
        .into_iter()
        .next()
        .and_then(|pod| pod.status)
        .and_then(|status| status.host_ip);
    let port = services
        .into_iter()
        .next()
        .and_then(|service| service.spec)
        .and_then(|spec| spec.ports)
        .and_then(|mut ports| ports.pop());
    Some(format!(
        "http://{}:{}",
        host_ip.unwrap(),
        port.unwrap().node_port.unwrap()
    ))
}

// kubectl get pods | grep http-echo
pub async fn get_http_echo_pod_name(client: &Client, namespace: &str) -> Option<String> {
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pods = pod_api
        .list(&ListParams::default().labels("app=http-echo"))
        .await
        .unwrap();
    let pod = pods.iter().next().and_then(|pod| pod.metadata.name.clone());
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
    watch_resource_exists(namespace_api, namespace).await;
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
    assert_eq!(res.status(), StatusCode::OK);
    // read all data sent back
    res.bytes().await.unwrap();
}

// kubectl apply -f tests/app.yaml -n name
pub async fn create_http_echo_pod(client: &Client, namespace: &str) {
    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": "http-echo",
            "labels": {
                "app": "http-echo"
            }
        },
        "spec": {
            "replicas": 1,
            "selector": {
                "matchLabels": {
                    "app": "http-echo"
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "app": "http-echo"
                    }
                },
                "spec": {
                    "containers": [
                        {
                            "name": "http-echo",
                            "image": "ealen/echo-server",
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
    watch_resource_exists(deployment_api, "http-echo").await;

    let service_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let service = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": "http-echo",
            "labels": {
                "app": "http-echo"
            }
        },
        "spec": {
            "type": "NodePort",
            "selector": {
                "app": "http-echo"
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
    watch_resource_exists(service_api, "http-echo").await;
}

// watch a resource until it exists
pub async fn watch_resource_exists<K: Debug + Clone + DeserializeOwned>(api: Api<K>, name: &str) {
    let params = ListParams::default()
        .fields(&format!("metadata.name={}", name))
        .timeout(10);
    let mut stream = api.watch(&params, "0").await.unwrap().boxed();
    while let Some(status) = stream.try_next().await.unwrap() {
        match status {
            WatchEvent::Modified(_) => break,
            WatchEvent::Error(s) => {
                panic!("Error watching namespaces: {:?}", s);
            }
            _ => {}
        }
    }
}

// Sends GET, POST, PUT, and DELETE requests to the given service URL -> Express/Flask server.
// PUT creates a file named "test" in cwd and DELETE deletes it.
pub async fn send_requests(service_url: &str) {
    http_request(service_url, Method::GET).await;
    http_request(service_url, Method::POST).await;

    http_request(service_url, Method::PUT).await;

    http_request(service_url, Method::DELETE).await;
}

/// For all requests, the Express/Flask server prints "{request_name}: Request completed",
/// this is verified by reading the stdout of the server
pub async fn validate_requests(stdout: &mut BufReader<ChildStdout>) {
    let mut out = String::new();
    stdout.read_to_string(&mut out).await.unwrap();
    // Todo: change this assertions to assert_eq! when TCPClose is patched

    assert_contains!(out, "GET: Request completed");
    assert_contains!(out, "POST: Request completed");
    assert_contains!(out, "PUT: Request completed");
    assert_contains!(out, "DELETE: Request completed");
    let cwd = env::current_dir().unwrap();
    let delete_path = cwd.join("deletetest"); // 'deletetest' is created in cwd, by PUT and deleted by DELETE
    let exist_path = cwd.join("test"); // 'test' is created in cwd, by PUT and **not** deleted by DELETE
    assert!(exist_path.exists());
    assert!(!delete_path.exists());
}

pub async fn validate_no_requests(stdout: &mut BufReader<ChildStdout>) {
    let mut out = String::new();
    stdout.read_to_string(&mut out).await.unwrap();

    assert!(!out.contains("GET: Request completed"));
    assert!(!out.contains("POST: Request completed"));
    assert!(!out.contains("PUT: Request completed"));
    assert!(!out.contains("DELETE: Request completed"));
    let cwd = env::current_dir().unwrap();
    let path = cwd.join("deletetest");
    assert!(!path.exists()); // the API creates a file called 'test' in cwd, which should not exist
}

// initializes the test/runs the node process
pub async fn test_server_init(
    client: &Client,
    pod_namespace: &str,
    mut env: HashMap<&str, &str>,
    server: &str,
) -> Child {
    let pod_name = get_http_echo_pod_name(client, pod_namespace).await.unwrap();
    let command = SERVERS.get(server).unwrap().clone();
    // used by the CI, to load the image locally:
    // docker build -t test . -f mirrord-agent/Dockerfile
    // minikube load image test:latest
    env.insert("MIRRORD_AGENT_IMAGE", "test");
    env.insert("MIRRORD_CHECK_VERSION", "false");
    let server = start_server(&pod_name, command, env);
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
