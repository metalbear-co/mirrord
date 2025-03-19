#![cfg(feature = "operator")]

use std::time::Duration;

use kube::{api::ObjectMeta, Api};
use mirrord_operator::crd::profile::{
    FeatureAdjustment, FeatureChange, FeatureKind, MirrordProfile, MirrordProfileSpec,
};
use rstest::rstest;
use tempfile::NamedTempFile;

use crate::utils::{
    kube_client, port_forwarder::PortForwarder, service, Application, KubeService, ResourceGuard,
};

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn mirrord_profile_enforces_stealing(
    #[future] kube_client: kube::Client,
    #[future] service: KubeService,
) {
    let kube_client = kube_client.await;
    let service = service.await;
    let target_path = service.pod_container_target();

    // The `KubeService` resources are created in a random namespace,
    // which will be deleted after this test.
    // However, mirrord profiles are clusterwide, so they need a separate guard.
    let (_profile_guard, profile_name) = {
        let profile = MirrordProfile {
            metadata: ObjectMeta {
                name: Some(format!("test-profile-{}", service.name)),
                ..Default::default()
            },
            spec: MirrordProfileSpec {
                feature_adjustments: vec![FeatureAdjustment {
                    kind: FeatureKind::Incoming,
                    change: FeatureChange::Steal,
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            },
        };
        let (guard, profile) = ResourceGuard::create(
            Api::<MirrordProfile>::all(kube_client.clone()),
            &profile,
            true,
        )
        .await
        .unwrap();

        (guard, profile.metadata.name.unwrap())
    };

    let port_forwarder =
        PortForwarder::new(kube_client, &service.pod_name, &service.namespace, 80).await;
    let service_url = format!("http://{}", port_forwarder.address());

    // Before we start the session, verify that the service responds.
    let response = reqwest::get(&service_url)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(response.as_ref(), b"OK - GET: Request completed\n");

    let test_process = Application::PythonFlaskHTTP
        .run(&target_path, Some(&service.namespace), None, None)
        .await;
    test_process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    // The local application should mirror the traffic.
    let response = reqwest::get(&service_url)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(response.as_ref(), b"OK - GET: Request completed\n"); // got response from remote
    test_process
        .wait_for_line(Duration::from_secs(20), "GET: Request completed")
        .await; // local received the request
    std::mem::drop(test_process); // kill the app

    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
    let config = serde_json::json!({
        "profile": profile_name,
    });
    serde_json::to_writer_pretty(config_file.as_file_mut(), &config).unwrap();
    let test_process = Application::PythonFlaskHTTP
        .run(
            &target_path,
            Some(&service.namespace),
            Some(vec!["-f", config_file.path().to_str().unwrap()]),
            None,
        )
        .await;

    // The local application should steal the traffic.
    let response = reqwest::get(&service_url)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(response.as_ref(), b"GET"); // got response from local
    std::mem::drop(test_process); // kill the app
}
