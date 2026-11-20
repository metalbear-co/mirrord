use serde_json::json;

use super::{ClusterWake, WakeState, WakeStatus};

/// The JSON that operators send is the contract with every released CLI, so its shape must not
/// drift: the cluster is left out for an operator that is no multi-cluster primary, `replica` is
/// left out unless it is set, and each state names its kind.
#[test]
fn status_keeps_its_wire_shape() {
    let status = WakeStatus {
        clusters: vec![
            ClusterWake {
                cluster: None,
                replica: false,
                state: WakeState::Ready,
            },
            ClusterWake {
                cluster: Some("east".to_owned()),
                replica: true,
                state: WakeState::Failed {
                    message: "image pull failed".to_owned(),
                },
            },
            ClusterWake {
                cluster: Some("west".to_owned()),
                replica: true,
                state: WakeState::Reconnecting {
                    message: "the connection closed".to_owned(),
                },
            },
        ],
    };

    let encoded = serde_json::to_value(&status).unwrap();

    assert_eq!(
        encoded,
        json!({
            "clusters": [
                {"state": {"kind": "ready"}},
                {
                    "cluster": "east",
                    "replica": true,
                    "state": {"kind": "failed", "message": "image pull failed"},
                },
                {
                    "cluster": "west",
                    "replica": true,
                    "state": {"kind": "reconnecting", "message": "the connection closed"},
                },
            ]
        })
    );
    assert_eq!(
        serde_json::from_value::<WakeStatus>(encoded).unwrap(),
        status
    );
}

/// An operator newer than the CLI can report a state the CLI doesn't know, which must not make
/// the whole status unreadable.
#[test]
fn unknown_state_reads_as_unknown() {
    let status: WakeStatus = serde_json::from_value(json!({
        "clusters": [{"cluster": "west", "state": {"kind": "hibernating", "since": 3}}]
    }))
    .unwrap();

    assert_eq!(
        status.clusters,
        [ClusterWake {
            cluster: Some("west".to_owned()),
            replica: false,
            state: WakeState::Unknown,
        }]
    );
}
