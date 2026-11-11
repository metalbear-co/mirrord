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
    tcp::{Filter, HttpBodyFilter, HttpFilter, JqQuery, JsonPathQuery, LayerTcpSteal, StealType},
};
use rstest::rstest;
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, net::TcpStream, time::timeout};

const BIND_ATTEMPTS: usize = 5;

// The agent prepends (?i) when compiling headers. Exercise that same regex engine and
// input format without exposing the agent binary's private matcher API.
fn session_regex(filter: HttpFilter, custom: Option<HttpFilter>) -> fancy_regex::Regex {
    let session = if let Some(custom) = custom {
        let HttpFilter::Composite {
            all: false,
            mut filters,
        } = filter
        else {
            panic!("custom expression must remain an independent OR branch");
        };
        assert_eq!(filters.len(), 2);
        let session = filters.pop().unwrap();
        assert_eq!(filters.pop().unwrap(), custom);
        session
    } else {
        filter
    };
    let HttpFilter::Header(pattern) = session else {
        panic!("expected baggage header filter")
    };
    // A `MirrordPolicy` header filter is a regex matched against this pattern, so a policy written
    // for the documented filter, `^baggage: .*mirrord-session=<key>.*$`, must accept it.
    let documented_policy = fancy_regex::Regex::new(r"^\^baggage: .*mirrord-session=").unwrap();
    assert!(
        documented_policy.is_match(&pattern.to_string()).unwrap(),
        "{pattern}"
    );
    fancy_regex::Regex::new(&format!("(?i){pattern}")).unwrap()
}

fn assert_fixture_session(filter: HttpFilter, custom: Option<HttpFilter>) {
    let regex = session_regex(filter, custom);
    for (header, expected) in [
        ("baggage: mirrord-session=dev.[A]+", true),
        ("BAGGAGE: mirrord-session=dev.[A]+", true),
        (
            "baggage: other=1, mirrord-session=dev.[A]+;prop=yes, next=2",
            false,
        ),
        ("baggage: mirrord-session=dev.%5BA%5D%2B", false),
        ("baggage: mirrord-session=dev.[a]+", false),
        ("baggage: mirrord-session=dev.[A]+suffix", false),
        ("baggage: other=dev.[A]+", false),
    ] {
        assert_eq!(regex.is_match(header).unwrap(), expected, "{header}");
    }
}

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

/// In a shared session, a custom AST must survive session widening, and filter scope follows the
/// mapped remote port.
#[rstest]
#[case::body("session_body.toml", HttpFilter::Body(HttpBodyFilter::Json {
    query: JsonPathQuery::new_unchecked("$.selected".to_owned()),
    matches: Filter::new("^true$".to_owned()).unwrap(),
}))]
#[case::jq("session_jq.toml", HttpFilter::HeaderJq(JqQuery::new("contains(\"x-team: local\")").unwrap()))]
#[case::composite("session_composite.toml", HttpFilter::Composite {
    all: true,
    filters: vec![
        HttpFilter::Path(Filter::new("^/custom$".to_owned()).unwrap()),
        HttpFilter::Body(HttpBodyFilter::Json {
            query: JsonPathQuery::new_unchecked("$.selected".to_owned()),
            matches: Filter::new("^true$".to_owned()).unwrap(),
        }),
        HttpFilter::HeaderJq(JqQuery::new("contains(\"x-team: local\")").unwrap()),
    ],
})]
#[tokio::test]
async fn filter_ports_preserves_custom_ast_and_mapping(
    config_dir: &std::path::Path,
    #[case] fixture: &str,
    #[case] custom: HttpFilter,
) {
    let (_process, mut intproxy) = Application::RustListenPorts
        .start_process(
            vec![("APP_PORTS", "54321")],
            Some(&config_dir.join(fixture)),
        )
        .await;
    let ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(
        23456,
        filter,
    ))) = intproxy.recv().await
    else {
        panic!("expected mapped filtered subscription")
    };
    assert_fixture_session(filter, Some(custom));
}

/// A shared session's subscription must select exactly the requests whose `baggage` header is
/// `mirrord-session=<key>`, which is how share-ingress tags them, and keep keys apart: a longer
/// key, a key with regex metacharacters, another case, and the key among other members.
/// This fake-agent harness checks the generated regex, not cluster request routing.
#[rstest]
#[case("alice")]
#[case("Alice")]
#[case("alice.*")]
#[case("a=b")]
#[case("")]
#[tokio::test]
async fn filter_ports_session_key_wire_contract(#[case] key: &str) {
    let config = json!({
        "key": key,
        "share": true,
        "feature": {"network": {"incoming": {
            "mode": "steal",
            "ports": [54321],
            "http_filter": {"path_filter": "^/independent$"}
        }}}
    });
    let mut config_file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    config_file
        .write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();
    let (_process, mut intproxy) = Application::RustListenPorts
        .start_process(vec![("APP_PORTS", "54321")], Some(config_file.path()))
        .await;
    let ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(
        54321,
        filter,
    ))) = intproxy.recv().await
    else {
        panic!("expected filtered subscription")
    };
    let custom = HttpFilter::Path(Filter::new("^/independent$".to_owned()).unwrap());
    // Check the complete OR composition, then its independently matching custom path.
    let HttpFilter::Composite { filters, .. } = &filter else {
        panic!("expected custom OR session")
    };
    let Some(HttpFilter::Path(path)) = filters.first() else {
        panic!("expected custom path")
    };
    let custom_regex = fancy_regex::Regex::new(&format!("(?i){path}")).unwrap();
    let session = session_regex(filter, Some(custom));
    // `selected_key` is the key of the header that share-ingress writes, `baggage:
    // mirrord-session=<key>`, or `None` for any other header.
    for (header, selected_key) in [
        ("baggage: mirrord-session=alice", Some("alice")),
        ("BAGGAGE: mirrord-session=alice", Some("alice")),
        ("baggage: mirrord-session=Alice", Some("Alice")),
        ("baggage: mirrord-session=alice.*", Some("alice.*")),
        ("baggage: mirrord-session=a=b", Some("a=b")),
        ("baggage: mirrord-session=", Some("")),
        ("baggage: mirrord-session=alice-suffix", None),
        ("baggage: mirrord-session=a", None),
        ("baggage: mirrord-session=alice ", None),
        ("baggage: mirrord-session= alice", None),
        // share-ingress replaces the header, so a shared session doesn't look for its key among
        // other members or properties.
        ("baggage: mirrord-session=alice;debug", None),
        ("baggage: mirrord-session=alice,debug=1", None),
        ("baggage: upstream=1,mirrord-session=alice", None),
        ("baggage: other=mirrord-session=alice", None),
        ("baggage: Mirrord-session=alice", None),
        ("baggage: mirrord-session =alice", None),
        // Receivers don't percent-decode the key, so an encoded spelling is another key.
        ("baggage: mirrord-session=%61lice", None),
    ] {
        let matches = session.is_match(header).unwrap();
        assert_eq!(
            matches,
            selected_key == Some(key),
            "key={key:?}, header={header:?}"
        );
        assert_eq!(
            matches || custom_regex.is_match("/unmatched").unwrap(),
            selected_key == Some(key)
        );
    }
}
