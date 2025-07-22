#![cfg(feature = "operator")]

use std::time::{Duration, Instant};

use kube::{api::ObjectMeta, Api};
use mirrord_operator::crd::profile::{
    FeatureAdjustment, FeatureChange, MirrordClusterProfile, MirrordClusterProfileSpec,
    MirrordProfile, MirrordProfileSpec,
};
use rstest::rstest;
use tempfile::NamedTempFile;

use crate::utils::{
    application::Application, kube_client, kube_service::KubeService,
    port_forwarder::PortForwarder, resource_guard::ResourceGuard, services::basic_service,
};

#[rstest]
#[case::namespaced(true)]
#[case::clusterwide(false)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn mirrord_profile_enforces_stealing(
    #[future] kube_client: kube::Client,
    #[future] basic_service: KubeService,
    #[case] namespaced_profile: bool,
) {
    let kube_client = kube_client.await;
    let service = basic_service.await;
    let deployed_at = Instant::now();
    let target_path = service.pod_container_target();

    let (_profile_guard, profile_name) = if namespaced_profile {
        let profile = MirrordProfile {
            metadata: ObjectMeta {
                // Service name is randomized, so there should be no conflict.
                name: Some(format!("test-profile-{}", service.name)),
                ..Default::default()
            },
            spec: MirrordProfileSpec {
                feature_adjustments: vec![FeatureAdjustment {
                    change: FeatureChange::IncomingSteal,
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            },
        };
        let (guard, profile) = ResourceGuard::create(
            Api::<MirrordProfile>::namespaced(kube_client.clone(), &service.namespace),
            &profile,
            true,
        )
        .await
        .unwrap();

        (guard, profile.metadata.name.unwrap())
    } else {
        let profile = MirrordClusterProfile {
            metadata: ObjectMeta {
                // Service name is randomized, so there should be no conflict.
                name: Some(format!("test-profile-{}", service.name)),
                ..Default::default()
            },
            spec: MirrordClusterProfileSpec {
                feature_adjustments: vec![FeatureAdjustment {
                    change: FeatureChange::IncomingSteal,
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            },
        };
        let (guard, profile) = ResourceGuard::create(
            Api::<MirrordClusterProfile>::all(kube_client.clone()),
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

    // Before we start the session, wait until the service is responsive.
    // TODO: remove when we add readiness probes to deployed containers.
    let response = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match reqwest::get(&service_url).await {
                Ok(response) => break response,
                Err(error) => {
                    println!(
                        "Failed to reach the remote service {}s after deployment: {error}",
                        deployed_at.elapsed().as_secs_f32()
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    })
    .await
    .unwrap();
    println!("Reached remote service, status code {}", response.status());
    let response = response.bytes().await.unwrap();
    assert_eq!(
        response.as_ref(),
        b"OK - GET: Request completed\n",
        "expected a response from the remote app, got: {}",
        String::from_utf8_lossy(response.as_ref())
    );

    // The local application should mirror the traffic.
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

    // With the cluster-wide or namespaced profile, the local application should steal the traffic.
    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
    let config = if namespaced_profile {
        serde_json::json!({
            "profile": format!("{}/{}", &service.namespace, profile_name),
        })
    } else {
        serde_json::json!({
            "profile": profile_name,
        })
    };
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
    std::mem::drop(test_process); // kill the app

    if namespaced_profile {
        // when target namespace is not set,
        // we should automatically use the namespaced profile's namespace
        let test_process = Application::PythonFlaskHTTP
            .run(
                &target_path,
                None,
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
}
