//! Share links: joining a session from a plain browser, with no mirrord extension installed.
//!
//! A share link is the app URL plus `?mirrord-session=<key>`. The agent turns that key into the
//! `baggage: mirrord-session=<key>` header the session's HTTP filter matches on, so the request
//! reaches the session owner's local process. To know which keys still have a live session, the
//! agent needs the operator to tell it - that is what [`ShareLinkRequest`] carries.

use std::sync::LazyLock;

use bincode::{Decode, Encode};
use semver::VersionReq;

/// Minimal mirrord-protocol version that allows [`ClientMessage::ShareLink`].
///
/// [`ClientMessage::ShareLink`]: crate::ClientMessage::ShareLink
pub static SHARE_LINK_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.29.0".parse().expect("Bad Identifier"));

/// Updates the set of session keys the agent accepts from share links.
///
/// Sent by the operator, which owns the sessions and therefore knows which keys are live. The
/// agent outlives single sessions (multiple clients share one agent), so keys arrive and leave
/// over its whole lifetime rather than being fixed at startup.
///
/// Keys are scoped to the connection that registers them: when it closes, the agent drops its
/// keys. An operator that dies cannot send [`RemoveKey`](Self::RemoveKey), so this is what keeps
/// a long-lived agent from serving keys of sessions that are gone. After reconnecting, register
/// the keys again, the same way port subscriptions are replayed.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum ShareLinkRequest {
    /// This key has a live session: requests carrying it should join that session.
    ///
    /// Registering a key this connection already registered changes nothing.
    RegisterKey(String),
    /// This key's session is gone: viewers still holding a link or cookie for it should be told
    /// the session ended instead of being routed to it.
    RemoveKey(String),
}
