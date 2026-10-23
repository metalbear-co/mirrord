//! The mirrord sessions running on this machine, which "only my sessions" narrows every screen to.
//!
//! Each running session announces itself over a socket in mirrord's session registry
//! (`~/.mirrord/sessions`), the same source `mirrord session list` reads. What is kept here is what
//! ties a session to what it made in the cluster: its id (the operator's session id) and its
//! session key (what queue splits, database branches and copies are made for).
//!
//! Resources carry those in different places: a session's `spec.session.id`, a queue split's
//! `spec.session`, an SQS session's `spec.sessionId`, a database branch's
//! `status.sessionInfo[*].id` and its `mirrord-session-key` label (the key, hashed), a copy's
//! `session_key`, a preview's `spec.key`. Rather than one rule per kind, a resource is taken to
//! belong to these sessions when any value in its labels, spec or status is one of their ids, keys
//! or hashed keys. That covers every kind the same way, the generic Resources screen included,
//! and new kinds without changes. Kafka temporary topics and queue registries carry no link at all.

use std::{collections::HashSet, sync::Arc, time::Duration};

use mirrord_config::config::EnvKey;
use mirrord_session_monitor_client::sessions_dir;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Notify, watch};

use crate::screens::terminal::sessions::read_all;

/// How often the registry is read again: sessions start and stop as the user works.
const REFRESH: Duration = Duration::from_secs(5);

/// The local sessions' ids and keys.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalSessions {
    ids: HashSet<String>,
    keys: HashSet<String>,
    /// The keys as resources label them: hashed, to fit in a label value.
    hashed_keys: HashSet<String>,
}

impl LocalSessions {
    /// Reads the local session registry.
    pub async fn load() -> Self {
        let Some(dir) = sessions_dir() else {
            return Self::default();
        };
        let mut sessions = Self::default();
        for info in read_all(&dir).await {
            sessions.ids.insert(info.session_id);
            if let Some(key) = info.key {
                sessions
                    .hashed_keys
                    .insert(EnvKey::to_hashed_label_value(&key));
                sessions.keys.insert(key);
            }
        }
        sessions
    }

    /// Keeps `sender` up to date with the registry, for as long as anyone listens, asking for a
    /// redraw whenever it changes.
    pub async fn watch(sender: watch::Sender<Option<LocalSessions>>, redraw: Arc<Notify>) {
        loop {
            let sessions = Self::load().await;
            if sender.send_if_modified(|current| {
                let changed = current.as_ref() != Some(&sessions);
                if changed {
                    *current = Some(sessions);
                }
                changed
            }) {
                redraw.notify_one();
            }
            if sender.is_closed() {
                return;
            }
            tokio::time::sleep(REFRESH).await;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.keys.is_empty()
    }

    /// How many sessions run here.
    pub fn count(&self) -> usize {
        self.ids.len()
    }

    /// Whether `value`, an id or a key read off a cluster resource, belongs to one of them.
    pub fn owns(&self, value: &str) -> bool {
        self.ids.contains(value) || self.keys.contains(value) || self.hashed_keys.contains(value)
    }

    /// Whether a resource belongs to one of these sessions: any value in its labels, spec or
    /// status is one of their ids, keys or hashed keys. See the module docs.
    pub fn owns_resource(&self, resource: &impl Serialize) -> bool {
        if self.is_empty() {
            return false;
        }
        let Ok(value) = serde_json::to_value(resource) else {
            return false;
        };
        let labels = value.pointer("/metadata/labels");
        [labels, value.get("spec"), value.get("status")]
            .into_iter()
            .flatten()
            .any(|part| self.mentioned_in(part))
    }

    fn mentioned_in(&self, value: &Value) -> bool {
        match value {
            Value::String(text) => self.owns(text),
            Value::Array(items) => items.iter().any(|item| self.mentioned_in(item)),
            // Keys too: some fields map session ids to what they hold.
            Value::Object(map) => map
                .iter()
                .any(|(key, value)| self.owns(key) || self.mentioned_in(value)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn mine() -> LocalSessions {
        LocalSessions {
            ids: HashSet::from(["3F2A9C01B4D5E6F7".to_owned()]),
            keys: HashSet::from(["alice-feature".to_owned()]),
            hashed_keys: HashSet::from([EnvKey::to_hashed_label_value("alice-feature")]),
        }
    }

    #[test]
    fn resources_belong_by_id_key_or_hashed_key_wherever_they_carry_it() {
        let sessions = mine();
        // A queue split names its session in `spec.session`.
        assert!(sessions.owns_resource(&json!({ "spec": { "session": "3F2A9C01B4D5E6F7" } })));
        // A database branch lists its users under `status.sessionInfo`, keyed by id.
        assert!(sessions.owns_resource(&json!({
            "status": { "sessionInfo": { "3F2A9C01B4D5E6F7": { "id": "3F2A9C01B4D5E6F7" } } }
        })));
        // Split-config branches carry the key hashed in a label.
        assert!(sessions.owns_resource(&json!({
            "metadata": { "labels": {
                "mirrord-session-key": EnvKey::to_hashed_label_value("alice-feature")
            } }
        })));
        // A copy carries the key itself.
        assert!(sessions.owns_resource(&json!({ "spec": { "session_key": "alice-feature" } })));
    }

    #[test]
    fn other_sessions_and_names_do_not_count() {
        let sessions = mine();
        assert!(!sessions.owns_resource(&json!({ "spec": { "session": "0000000000000001" } })));
        // The resource's own name is not a link, even when it happens to match a key.
        assert!(!sessions.owns_resource(&json!({ "metadata": { "name": "alice-feature" } })));
        // With no session running here, nothing belongs.
        assert!(!LocalSessions::default().owns_resource(&json!({ "spec": { "session": "" } })));
    }
}
