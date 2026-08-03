use std::borrow::Cow;

use serde::{Deserialize, Serialize};

#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum_macros::AsRefStr,
    strum_macros::Display,
)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
#[strum(serialize_all = "snake_case")]
pub enum PeerRegistration {
    Agent {
        replica_id: String,
    },
    Intproxy {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_replica_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterPayload {
    pub room_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(flatten)]
    pub registration: PeerRegistration,
}

impl RegisterPayload {
    pub fn role(&self) -> &str {
        self.registration.as_ref()
    }

    pub fn agent(
        room_id: impl Into<String>,
        replica_id: impl Into<String>,
        namespace: impl Into<Option<String>>,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            namespace: namespace.into(),
            registration: PeerRegistration::Agent {
                replica_id: replica_id.into(),
            },
        }
    }

    pub fn intproxy(
        room_id: impl Into<String>,
        session_id: impl Into<String>,
        target_replica_id: impl Into<Option<String>>,
        namespace: impl Into<Option<String>>,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            namespace: namespace.into(),
            registration: PeerRegistration::Intproxy {
                session_id: session_id.into(),
                target_replica_id: target_replica_id.into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConnectPayload {
    pub room_id: String,
    pub ws_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataplaneReadyPayload {
    pub room_id: String,
    pub ws_path: String,
}

#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum_macros::AsRefStr,
    strum_macros::Display,
    strum_macros::IntoStaticStr,
)]
#[strum(serialize_all = "lowercase")]
pub enum ControlPlaneMessages {
    // Sent by clients with RegisterPayload
    // register into a specific room
    Register,

    // Sent by control plane to each intproxy<->agent pair registered in a room to start
    // communicating over dataplane
    Handoff,
}

// for socketioxide .on(Event)
impl From<ControlPlaneMessages> for Cow<'static, str> {
    fn from(value: ControlPlaneMessages) -> Self {
        Cow::Owned(value.to_string())
    }
}

// for rust_socketio .on(Event)
#[cfg(feature = "client")]
impl From<ControlPlaneMessages> for rust_socketio::Event {
    fn from(value: ControlPlaneMessages) -> Self {
        value.to_string().into()
    }
}

#[cfg(test)]
mod tests {
    use super::{PeerRegistration, RegisterPayload};

    #[test]
    fn serializes_agent_registration_with_tagged_role() {
        let payload = RegisterPayload::agent("room", "replica", None);

        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({
                "room_id": "room",
                "role": "agent",
                "replica_id": "replica",
            })
        );
    }

    #[test]
    fn serializes_intproxy_registration_with_tagged_role() {
        let payload =
            RegisterPayload::intproxy("room", "session", Some("replica".to_owned()), None);

        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({
                "room_id": "room",
                "role": "intproxy",
                "session_id": "session",
                "target_replica_id": "replica",
            })
        );
    }

    #[test]
    fn serializes_registration_with_namespace() {
        let payload =
            RegisterPayload::intproxy("room", "session", None, Some("staging".to_owned()));

        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({
                "room_id": "room",
                "namespace": "staging",
                "role": "intproxy",
                "session_id": "session",
            })
        );
    }

    #[test]
    fn round_trips_registrations() {
        let agent = RegisterPayload::agent("room", "replica", None);
        let intproxy = RegisterPayload::intproxy("room", "session", None, None);

        assert_eq!(
            serde_json::from_value::<RegisterPayload>(serde_json::to_value(&agent).unwrap())
                .unwrap(),
            agent
        );
        assert_eq!(
            serde_json::from_value::<RegisterPayload>(serde_json::to_value(&intproxy).unwrap())
                .unwrap(),
            intproxy
        );
    }

    #[test]
    fn rejects_missing_required_fields() {
        let payload = serde_json::json!({
            "room_id": "room",
            "role": "agent",
        });

        assert!(serde_json::from_value::<RegisterPayload>(payload).is_err());
    }

    #[test]
    fn rejects_mixed_role_fields() {
        let payload = serde_json::json!({
            "room_id": "room",
            "role": "agent",
            "session_id": "session",
        });

        assert!(serde_json::from_value::<RegisterPayload>(payload).is_err());

        let payload = serde_json::json!({
            "room_id": "room",
            "role": "intproxy",
            "replica_id": "replica",
        });

        assert!(serde_json::from_value::<RegisterPayload>(payload).is_err());
    }

    #[test]
    fn peer_registration_round_trips() {
        let registration = PeerRegistration::Intproxy {
            session_id: "session".to_owned(),
            target_replica_id: Some("replica".to_owned()),
        };

        assert_eq!(
            serde_json::from_value::<PeerRegistration>(
                serde_json::to_value(&registration).unwrap()
            )
            .unwrap(),
            registration
        );
    }
}
