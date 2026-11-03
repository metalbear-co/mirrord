//! Messages between the intproxy and the operator for a session started with `share`.
//!
//! A shared session asks the operator for a share link: a host under the cluster's share domain
//! that share-ingress routes to the session's target. These messages travel on the session's
//! existing operator connection, after the agent protocol version check, and only when the
//! client asks for a link with [`LocalShareClientMessage::Start`]. The operator keeps the link
//! while the session's connection lives and the port that the link opens (the entry port) is
//! stolen, so neither side sends liveness or teardown messages of its own.

use std::{fmt::Write, sync::LazyLock};

use bincode::{Decode, Encode};
use semver::VersionReq;

/// The protocol version from which the operator understands these messages.
pub static LOCAL_SHARE_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.31.0".parse().expect("valid local share version"));

/// What the operator tells the intproxy about the share link.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum LocalShareEvent {
    /// The operator registered a link that opens `entry_port`. The link isn't usable until
    /// [`LocalShareEvent::Ready`], because the local application hasn't listened on that port
    /// yet.
    Accepted { entry_port: u16 },
    /// share-ingress now routes `url` to the session.
    Ready { url: String },
    /// The link failed and the operator removed it. `message` is meant for the user.
    Failed { message: String },
}

/// What the intproxy tells the operator about the share link.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum LocalShareClientMessage {
    /// Requests a share link for this session.
    Start,
}

/// Spells a session key as the value of the `mirrord-session` member of a W3C `baggage` header.
///
/// share-ingress puts this value on every request it forwards, and a shared session's HTTP filter
/// matches exactly this value, so both sides must use this function. Bytes that a baggage value
/// can't hold, and `%` itself, are percent-encoded, so any key reaches the filter intact: the key
/// `alice;debug` becomes `alice%3Bdebug`.
pub fn baggage_session_value(key: &str) -> String {
    let mut value = String::with_capacity(key.len());
    for byte in key.bytes() {
        if matches!(byte, 0x21 | 0x23..=0x24 | 0x26..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E)
        {
            value.push(char::from(byte));
        } else {
            write!(value, "%{byte:02X}").expect("writing into a String cannot fail");
        }
    }
    value
}
