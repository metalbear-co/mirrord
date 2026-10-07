#![cfg(target_family = "unix")]
#![cfg(target_os = "linux")]

mod common;

use core::assert_matches;
use std::{io::Write, net::TcpListener, ops::Not, time::Duration};

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
use tokio::{io::AsyncWriteExt, net::TcpStream, time::timeout};

const BIND_ATTEMPTS: usize = 5;

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
                .insert("ports".to_owned(), json!(ports));
        }

        // Insert the completed http_filter into incoming
        incoming
            .as_object_mut()
            .unwrap()
            .insert("http_filter".to_owned(), http_filter);
    }

    // Conditionally add top-level "ports" directly under "incoming"
    if let Some(ports) = incoming_ports {
        incoming
            .as_object_mut()
            .unwrap()
            .insert("ports".to_owned(), json!(ports));
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

/// Verifies which ports stay local, receive all traffic, or receive filtered HTTP traffic.
#[rstest]
#[tokio::test]
async fn filter_ports(
    // Reusing test app
    #[values(Application::RustListenPorts)] application: Application,

    #[values(
		None,
		Some(&[0][..]),
		Some(&[0, 1][..]),
		Some(&[1][..])
	)]
    incoming_port_offsets: Option<&[u16]>,

    #[values(
		None,
		Some(&[0][..]),
		Some(&[0, 1][..]),
		Some(&[1][..])
	)]
    http_filter_port_offsets: Option<&[u16]>,

    #[values(true, false)] have_filter: bool,
) {
    let mut attempt = 0;
    let (mut test_process, mut intproxy, port, behavior, _config_file) = loop {
        attempt += 1;
        let port = rand::random_range(10000..60000);
        // A replacement port must preserve the inclusion/filter case being tested.
        let ports_at = |offsets: Option<&[u16]>| {
            offsets.map(|offsets| {
                offsets
                    .iter()
                    .map(|offset| port + offset)
                    .collect::<Vec<_>>()
            })
        };
        let config = build_config(
            ports_at(incoming_port_offsets).as_deref(),
            ports_at(http_filter_port_offsets).as_deref(),
            have_filter,
        );
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
        let behavior = expected_behavior(port, &incoming_config);

        let (mut test_process, intproxy) = application
            .start_process(
                vec![("APP_PORTS", &port.to_string())],
                Some(config_file.path()),
            )
            .await;

        if matches!(behavior, BindMode::Local) {
            // A partial line for port 12345 must not match port 1234.
            test_process
                .wait_for_line_stdout(Duration::from_secs(5), &format!("PORT {port}\n"))
                .await;
            if test_process
                .get_stdout()
                .await
                .contains(&format!("AddrInUse PORT {port}\n"))
            {
                timeout(Duration::from_secs(5), test_process.wait_assert_fail())
                    .await
                    .expect("application did not exit after AddrInUse");
                assert!(
                    attempt < BIND_ATTEMPTS,
                    "application could not bind port {port} after {BIND_ATTEMPTS} attempts (AddrInUse)"
                );
                continue;
            }
        }

        break (test_process, intproxy, port, behavior, config_file);
    };

    match behavior {
        BindMode::Local => {
            test_process
                .assert_stdout_contains(&format!("LISTENING PORT {port}\n"))
                .await;
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream.write_all(b"HELLO").await.unwrap();
            stream.shutdown().await.unwrap();

            timeout(Duration::from_secs(5), test_process.wait_assert_success())
                .await
                .expect("application did not exit after HELLO");
            test_process.assert_no_error_in_stderr().await;
            test_process.assert_no_error_in_stdout().await;
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
}

/// Excluded ports use real local binds, so an occupied port must not fall back to another address.
#[tokio::test]
async fn excluded_port_in_use_fails_bind() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    // An empty whitelist excludes any port, including u16::MAX, without adding an offset.
    let config = build_config(Some(&[]), None, false);
    let mut config_file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    config_file
        .as_file_mut()
        .write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();

    let (mut test_process, _intproxy) = Application::RustListenPorts
        .start_process(
            vec![("APP_PORTS", &port.to_string())],
            Some(config_file.path()),
        )
        .await;
    test_process
        .wait_for_line_stdout(Duration::from_secs(5), &format!("AddrInUse PORT {port}\n"))
        .await;
    timeout(Duration::from_secs(5), test_process.wait_assert_fail())
        .await
        .expect("application did not exit after AddrInUse");
}
