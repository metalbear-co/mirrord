use k8s_openapi::api::{apps::v1::Deployment, core::v1::Service};
use serde_json::{json, Value};

use crate::utils::{CONTAINER_NAME, TEST_RESOURCE_LABEL};

pub(crate) mod operator;

pub(super) fn deployment_from_json(name: &str, image: &str, env: Value) -> Deployment {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-deployments": format!("deployment-{name}")
            }
        },
        "spec": {
            "replicas": 1,
            "selector": {
                "matchLabels": {
                    "app": &name
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "containers": [
                        {
                            "name": &CONTAINER_NAME,
                            "image": image,
                            "ports": [
                                {
                                    "containerPort": 80
                                }
                            ],
                            "env": env,
                        }
                    ]
                }
            }
        }
    }))
    .expect("Failed creating `deployment` from json spec!")
}

pub(super) fn service_from_json(name: &str, service_type: &str) -> Service {
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
            }
        },
        "spec": {
            "type": service_type,
            "selector": {
                "app": name
            },
            "sessionAffinity": "None",
            "ports": [
                {
                    "name": "http",
                    "protocol": "TCP",
                    "port": 80,
                    "targetPort": 80,
                },
                {
                    "name": "udp",
                    "protocol": "UDP",
                    "port": 31415,
                },
            ]
        }
    }))
    .expect("Failed creating `service` from json spec!")
}
