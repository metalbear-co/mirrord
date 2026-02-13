use k8s_openapi::{
    api::{
        apps::v1::{Deployment, StatefulSet},
        core::v1::{EnvFromSource, Probe, Service, TCPSocketAction},
    },
    apimachinery::pkg::util::intstr::IntOrString,
    DeepMerge, Resource,
};
use kube::runtime::reflector::Lookup;
use mirrord_kube::api::kubernetes::rollout::Rollout;
use serde_json::{json, Value};

use crate::utils::{CONTAINER_NAME, TEST_RESOURCE_LABEL};

pub(crate) mod operator;

/// List of test images that run a TCP server on port 80.
///
/// For deployed test targets using these images, we will use a TCP startup probe.
///
/// See [test images repo](https://github.com/metalbear-co/test-images) for reference.
const TCP_SERVER_IMAGES: &[&str] = &[
    "ghcr.io/metalbear-co/mirrord-pytest:latest",
    "ghcr.io/metalbear-co/mirrord-node:latest",
    "ghcr.io/metalbear-co/mirrord-http-logger:latest",
    "ghcr.io/metalbear-co/mirrord-tcp-echo:latest",
    "ghcr.io/metalbear-co/mirrord-websocket:latest",
    "ghcr.io/metalbear-co/mirrord-http-keep-alive:latest",
];

pub(super) fn get_pod_template_json_value(
    name: &str,
    image: &str,
    env: Value,
    env_from: Option<Vec<EnvFromSource>>,
) -> Value {
    let use_probe = TCP_SERVER_IMAGES.contains(&image);
    json!({
        "metadata": {
            "labels": {
                "app": name,
                "test-label-for-pods": format!("pod-{name}"),
                format!("test-label-for-pods-{name}"): name
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
                    "envFrom": serde_json::to_value(env_from).unwrap(),
                    "startupProbe": use_probe.then_some(Probe {
                        tcp_socket: Some(TCPSocketAction {
                            host: None,
                            port: IntOrString::Int(80),
                        }),
                        ..Default::default()
                    }),
                }
            ]
        }
    })
}

pub(super) fn deployment_from_json(
    name: &str,
    image: &str,
    env: Value,
    env_from: Option<Vec<EnvFromSource>>,
    replicas: u8,
) -> Deployment {
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
            "replicas": replicas,
            "selector": {
                "matchLabels": {
                    "app": &name
                }
            },
            "template": get_pod_template_json_value(name, image, env, env_from)
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

pub(super) enum SpecSource<'a> {
    WorkloadRef(&'a Deployment),
    PodTemplate(Value),
}

impl SpecSource<'_> {
    fn get_workload_ref(&self) -> Value {
        match self {
            SpecSource::WorkloadRef(dep) => json!({
                "apiVersion": Deployment::API_VERSION,
                "kind": Deployment::KIND,
                "name": dep.name().unwrap(),
                "scaleDown": "progressively"
            }),
            SpecSource::PodTemplate(_) => Value::Null,
        }
    }

    fn get_pod_template(self) -> Value {
        match self {
            SpecSource::WorkloadRef(_) => Value::Null,
            SpecSource::PodTemplate(value) => value,
        }
    }
}

/// Creates an Argo Rollout resource with the given name, image, and env vars
///
/// Creates a [`Rollout`] resource following the Argo Rollouts
/// [specification](https://argoproj.github.io/argo-rollouts/features/specification/)
pub(super) fn argo_rollout_from_json(name: &str, spec_source: SpecSource) -> Rollout {
    serde_json::from_value(json!({
        "apiVersion": Rollout::API_VERSION,
        "kind": Rollout::KIND,
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-rollouts": format!("rollout-{name}")
            }
        },
        "spec": {
            "replicas": 1,
            "selector": {
                "matchLabels": {
                    "app": name
                }
            },
            "workloadRef": spec_source.get_workload_ref(),
            "template": spec_source.get_pod_template(),
            "strategy": {
                "canary": {}
            }
        }
    }))
    .expect("Failed creating `rollout` from json spec!")
}

pub fn stateful_set_from_json(name: &str, image: &str, has_pvc: bool) -> StatefulSet {
    let mut set: StatefulSet = serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-statefulsets": format!("statefulset-{name}")
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
                            "env": [
                                {
                                  "name": "MIRRORD_FAKE_VAR_FIRST",
                                  "value": "mirrord.is.running"
                                },
                                {
                                  "name": "MIRRORD_FAKE_VAR_SECOND",
                                  "value": "7777"
                                },
                                {
                                    "name": "MIRRORD_FAKE_VAR_THIRD",
                                    "value": "foo=bar"
                                }
                            ],
                        }
                    ]
                }
            }
        }
    }))
    .expect("Failed creating `statefulset` from json spec!");
    if has_pvc {
        let volume_claims = serde_json::from_value(json!({
            "spec": {
                "containers": [
                    {
                        "name": &CONTAINER_NAME,
                        "volumeMounts": [
                            {
                                "name": "data",
                                "mountPath": "/data"
                            }
                        ]
                    }
                ],
                "volumeClaimTemplates": [
                {
                    "metadata": {
                        "name": "data"
                    },
                    "spec": {
                        "accessModes": [
                            "ReadWriteOnce"
                        ],
                        "resources": {
                            "requests": {
                                "storage": "1Ki"
                            }
                        }
                    }
                }
            ]
            }
        }))
        .expect("Failed creating `statefulset.spec` with `volumeClaimTemplates` from json spec!");
        set.merge_from(volume_claims);
    }
    set
}
