#![cfg(target_family = "unix")]
#![cfg(target_os = "linux")]
#![feature(assert_matches)]

mod common;

use std::{assert_matches::assert_matches, io::Write, ops::Not, path::Path, time::Duration};

pub use common::*;
use mirrord_config::{
    LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
    feature::network::incoming::IncomingConfig,
};
use mirrord_protocol::{
    ClientMessage,
    tcp::{Filter, HttpFilter, LayerTcpSteal, StealType},
};
use rstest::rstest;
use serde_json::{Value, json};
use tokio::net::TcpStream;

fn build_config(
    incoming_ports: Option<&[u16]>,
    filter_ports: Option<&[u16]>,
    have_filter: bool,
) -> Value {
    // Start with the innermost fixed part: the "incoming" object
    let mut incoming = json!({
        "mode": "steal"
    });

    // Conditionally add the entire "http_filter" object if have_filter is true
    if have_filter {
        // Build the http_filter object
        let mut http_filter = json!({
            "path_filter": "/test"
        });

        // Conditionally add "ports" inside http_filter
        if let Some(ports) = filter_ports {
            http_filter
                .as_object_mut()
                .unwrap()
                .insert("ports".to_string(), json!(ports));
        }

        // Insert the completed http_filter into incoming
        incoming
            .as_object_mut()
            .unwrap()
            .insert("http_filter".to_string(), http_filter);
    }

    // Conditionally add top-level "ports" directly under "incoming"
    if let Some(ports) = incoming_ports {
        incoming
            .as_object_mut()
            .unwrap()
            .insert("ports".to_string(), json!(ports));
    }

    // Now build the full config using the modified incoming object
    json!({
        "feature": {
            "network": {
                "incoming": incoming
            }
        }
    })
}

enum BindMode {
    Local,
    Unfiltered,
    Filtered,
}

fn expected_behavior(port: u16, incoming: &IncomingConfig) -> BindMode {
    if let Some(remote_ports) = &incoming.ports
        && remote_ports.contains(&port).not()
    {
        return BindMode::Local;
    }

    if incoming.http_filter.is_filter_set().not() {
        return BindMode::Unfiltered;
    };

    if let Some(filtered_ports) = &incoming.http_filter.ports
        && filtered_ports.contains(&port).not()
    {
        BindMode::Unfiltered
    } else {
        BindMode::Filtered
    }
}

/// Verifies that the layer respects `feature.network.incoming.listen_ports` mapping.
#[rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(60))]
async fn filter_ports(
    // Reusing test app
    #[values(Application::RustListenPorts)] application: Application,
    dylib_path: &Path,

    #[values(rand::random_range(10000..60000))] port: u16,

    #[values(
		None,
		Some(&[port][..]),
		Some(&[port, port + 1][..]),
		Some(&[port + 1][..])
	)]
    incoming_ports: Option<&[u16]>,

    #[values(
		None,
		Some(&[port][..]),
		Some(&[port, port + 1][..]),
		Some(&[port + 1][..])
	)]
    http_filter_ports: Option<&[u16]>,

    #[values(true, false)] have_filter: bool,
) {
    let config = build_config(incoming_ports, http_filter_ports, have_filter);
    let mut config_file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    config_file
        .as_file_mut()
        .write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();

    let mut ctx = ConfigContext::default();
    let config_parsed = LayerFileConfig::from_path(&config_file, &mut ctx)
        .unwrap()
        .generate_config(&mut ctx)
        .unwrap();

    let incoming_config = config_parsed.feature.network.incoming;

    let (test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("APP_PORTS", &port.to_string())],
            Some(config_file.path()),
        )
        .await;

    match expected_behavior(port, &incoming_config) {
        BindMode::Local => {
            // Wait a little for test process
            tokio::time::sleep(Duration::from_millis(250)).await;
            let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            drop(stream);
        }
        BindMode::Unfiltered => {
            assert_matches!(
                intproxy.recv().await,
                ClientMessage::TcpSteal(
                    LayerTcpSteal::PortSubscribe(StealType::All(stolen_port))
                ) if stolen_port == port
            );
        }
        BindMode::Filtered => {
            assert_matches!(
                intproxy.recv().await,
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(
                    stolen_port,
                    HttpFilter::Path(filter)
                ))) if filter == Filter::new("/test".into()).unwrap() && stolen_port == port
            );
        }
    }

    drop(test_process)
}
