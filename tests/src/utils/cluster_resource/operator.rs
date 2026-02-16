#![cfg(feature = "operator")]

use k8s_openapi::api::batch::v1::{CronJob, Job};
use serde_json::json;

use crate::utils::{CONTAINER_NAME, TEST_RESOURCE_LABEL};

pub(crate) fn cron_job_from_json(name: &str, image: &str) -> CronJob {
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "CronJob",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-cronjobs": format!("cronjob-{name}")
            }
        },
        "spec": {
            "schedule": "* * * * *",
            "concurrencyPolicy": "Forbid",
            "jobTemplate": {
                "metadata": {
                    "labels": {
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "template": {
                        "spec": {
                            "restartPolicy": "OnFailure",
                            "containers": [
                                {
                                    "name": &CONTAINER_NAME,
                                    "image": image,
                                    "ports": [{ "containerPort": 80 }],
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
                                    ]
                                }
                            ]
                        }
                    }
                }
            }
        }
    }))
    .expect("Failed creating `cronjob` from json spec!")
}

pub(crate) fn job_from_json(name: &str, image: &str) -> Job {
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-jobs": format!("job-{name}")
            }
        },
        "spec": {
            "ttlSecondsAfterFinished": 10,
            "backoffLimit": 1,
            "template": {
                "metadata": {
                    "labels": {
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "restartPolicy": "OnFailure",
                    "containers": [
                        {
                            "name": &CONTAINER_NAME,
                            "image": image,
                            "ports": [{ "containerPort": 80 }],
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
            },
        }
    }))
    .expect("Failed creating `job` from json spec!")
}
