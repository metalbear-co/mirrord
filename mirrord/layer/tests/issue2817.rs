#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::{
    tcp::{Filter, HttpFilter, LayerTcpSteal, StealType},
    ClientMessage,
};
use rand::Rng;
use rstest::rstest;

mod common;

pub use common::*;

#[derive(Clone, Copy, Debug)]
enum TestedStealVariant {
    Unfiltered,
    Header,
    Path,
    AllOf,
    AnyOf,
}

impl TestedStealVariant {
    fn as_json_config(self) -> serde_json::Value {
        serde_json::json!({
            "path_filter": matches!(self, Self::Path).then(|| "/some/path"),
            "header_filter": matches!(self, Self::Header).then(|| "some: header"),
            "all_of": matches!(self, Self::AllOf).then(|| serde_json::json!([
                { "path": "/some/path" },
                { "header": "some: header" },
            ])),
            "any_of": matches!(self, Self::AnyOf).then(|| serde_json::json!([
                { "path": "/some/path" },
                { "header": "some: header" },
            ]))
        })
    }

    fn assert_matches(self, steal_type: StealType) {
        match (self, steal_type) {
            (Self::Unfiltered, StealType::All(80)) => {}
            (Self::Header, StealType::FilteredHttpEx(80, HttpFilter::Header(filter))) => {
                assert_eq!(filter, Filter::new("some: header".into()).unwrap())
            }
            (Self::Path, StealType::FilteredHttpEx(80, HttpFilter::Path(filter))) => {
                assert_eq!(filter, Filter::new("/some/path".into()).unwrap())
            }
            (
                Self::AllOf,
                StealType::FilteredHttpEx(80, HttpFilter::Composite { all: true, filters }),
            ) => {
                assert_eq!(
                    filters,
                    vec![
                        HttpFilter::Path(Filter::new("/some/path".into()).unwrap()),
                        HttpFilter::Header(Filter::new("some: header".into()).unwrap()),
                    ],
                )
            }
            (
                Self::AnyOf,
                StealType::FilteredHttpEx(
                    80,
                    HttpFilter::Composite {
                        all: false,
                        filters,
                    },
                ),
            ) => {
                assert_eq!(
                    filters,
                    vec![
                        HttpFilter::Path(Filter::new("/some/path".into()).unwrap()),
                        HttpFilter::Header(Filter::new("some: header".into()).unwrap()),
                    ],
                )
            }
            (.., steal_type) => panic!("received unexpected steal type: {steal_type:?}"),
        }
    }
}

/// Verify that the layer requests correct subscription type based on HTTP filter config.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2817(
    #[values(Application::NodeHTTP)] application: Application,
    dylib_path: &Path,
    #[values(
        TestedStealVariant::Unfiltered,
        TestedStealVariant::Header,
        TestedStealVariant::Path,
        TestedStealVariant::AllOf,
        TestedStealVariant::AnyOf
    )]
    config_variant: TestedStealVariant,
) {
    let dir = tempfile::tempdir().unwrap();
    let file_id = rand::thread_rng().gen::<u64>();
    let config_path = dir.path().join(format!("{file_id:X}.json"));

    let config = serde_json::json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "steal",
                    "http_filter": config_variant.as_json_config(),
                }
            }
        }
    });

    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .await
        .expect("failed to saving layer config to tmp file");

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            Default::default(),
            Some(config_path.to_str().unwrap()),
        )
        .await;

    let message = intproxy.recv().await;
    let steal_type = match message {
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(steal_type)) => steal_type,
        other => panic!("unexpected message received from the app: {other:?}"),
    };
    config_variant.assert_matches(steal_type);

    test_process
        .child
        .kill()
        .await
        .expect("failed to kill the app");
}
