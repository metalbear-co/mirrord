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

    let (_profile_guard, profile_name) = {
        let profile = MirrordProfile {
            metadata: ObjectMeta {
                // Service name is randomized, so there should be no conflict.
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
    assert_eq!(
        response.as_ref(),
        b"OK - GET: Request completed\n",
        "expected a response from the remote app, got: {}",
        String::from_utf8_lossy(response.as_ref())
    );

    // The local application should mirrord the traffic.
    let test_process = Application::PythonFlaskHTTP
        .run(&target_path, Some(&service.namespace), None, None)
        .await;
    test_process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;
    let response = reqwest::get(&service_url)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        response.as_ref(),
        b"OK - GET: Request completed\n",
        "expected response from the remote app, got: {}",
        String::from_utf8_lossy(response.as_ref())
    );
    test_process
        .wait_for_line_stdout(Duration::from_secs(40), "GET: Request completed")
        .await; // verify that the local app received the request as well
    std::mem::drop(test_process); // kill the app

    // With the profile, the local application should steal the traffic.
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
    test_process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;
    let response = reqwest::get(&service_url)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        response.as_ref(),
        b"GET",
        "expected response from the local app, got: {}",
        String::from_utf8_lossy(response.as_ref())
    );
}
