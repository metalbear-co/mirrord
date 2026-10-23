//! "Only my sessions" for list screens: what the `m` switch narrows them to, and how a screen says
//! that it is narrowed.
//!
//! The switch is one for the whole interface, so every screen narrows the same way: rows stay
//! when they belong to a mirrord session running on this machine (see [`LocalSessions`] for how a
//! resource is tied to one), and the panel title and the empty-list message say the list is
//! narrowed, so an empty list is never mistaken for an empty cluster.

use serde::Serialize;
use tokio::sync::watch;

use crate::{context::Context, kit::state_view::EmptyText, local_sessions::LocalSessions};

/// What an empty list says while the switch is on.
const NOTHING_MINE: &str = "Nothing from the sessions running here - m shows everything";

/// The switch and the sessions it narrows to, as a screen follows them.
#[derive(Clone)]
pub struct SessionScope {
    local_only: watch::Receiver<bool>,
    sessions: watch::Receiver<Option<LocalSessions>>,
}

impl SessionScope {
    pub fn new(context: &Context) -> Self {
        Self {
            local_only: context.local_only.clone(),
            sessions: context.local_sessions.clone(),
        }
    }

    /// The sessions to narrow to while the switch is on, else `None`. With the switch on and no
    /// session running here, the sessions are empty, so lists show nothing rather than everything.
    pub fn current(&self) -> Option<LocalSessions> {
        if !*self.local_only.borrow() {
            return None;
        }
        Some(self.sessions.borrow().clone().unwrap_or_default())
    }
}

/// Whether `resource` stays on screen: always with the switch off, else only when it belongs to
/// one of the sessions.
pub fn keeps(filter: &Option<LocalSessions>, resource: &impl Serialize) -> bool {
    filter
        .as_ref()
        .is_none_or(|sessions| sessions.owns_resource(resource))
}

/// The panel note, `updated 13:01:02`, with `only mine · ` in front while the switch is on.
pub fn note(filter: &Option<LocalSessions>, updated: &str) -> String {
    match filter {
        Some(_) => format!("only mine \u{b7} {updated}"),
        None => updated.to_owned(),
    }
}

/// Whether the list is narrowed by the switch or the search, which decides between "nothing
/// here" and "nothing matches" when it is empty.
pub fn narrowed(filter: &Option<LocalSessions>, searching: bool) -> bool {
    searching || filter.is_some()
}

/// The empty-list messages, with the switch's own when it, not the search, emptied the list.
pub fn empty_text(filter: &Option<LocalSessions>, searching: bool, base: EmptyText) -> EmptyText {
    match (filter, searching) {
        (Some(_), false) => EmptyText {
            no_match: NOTHING_MINE,
            ..base
        },
        _ => base,
    }
}
