use std::borrow::Cow;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Agent,
    Intproxy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterPayload {
    pub role: Role,
    pub room_id: String,
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
