use std::collections::HashSet;

/// Local mirrord session registry.
#[derive(Debug, Clone)]
pub struct LocalSessions {
    ids: HashSet<String>,
}

impl LocalSessions {
    /// Loads the local session registry.
    #[expect(unused, reason = "Not yet implemented.")]
    pub async fn load() -> Self {
        // TODO: Implement

        Self {
            ids: HashSet::new(),
        }
    }

    /// Returns the set of local session IDs.
    #[allow(unused, reason = "Nothing uses this yet.")]
    pub fn ids(&self) -> &HashSet<String> {
        &self.ids
    }
}
