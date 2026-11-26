//! The namespace listing the operator serves under its own API.
//!
//! With a kubeconfig, `mirrord ls` reads the cluster's `Namespace` resources itself. A CLI logged
//! in through SSO reaches only `/apis/operator.metalbear.co/`, so it asks the operator, which
//! answers with the namespaces the caller may list targets in: every namespace the operator serves
//! where the cluster's RBAC lets this user `list` targets.

use serde::{Deserialize, Serialize};

/// Path under the operator API that lists the namespaces the caller may list targets in.
pub fn namespaces_api_path() -> &'static str {
    "/apis/operator.metalbear.co/v1/namespaces"
}

/// What `GET` on [`namespaces_api_path`] answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNamespaceList {
    /// Sorted, without duplicates. Empty when RBAC lets the caller list targets nowhere.
    #[serde(default)]
    pub items: Vec<OperatorNamespace>,
}

/// One namespace in an [`OperatorNamespaceList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNamespace {
    pub name: String,
}

impl OperatorNamespaceList {
    /// The namespace names, in the operator's order.
    pub fn names(self) -> Vec<String> {
        self.items.into_iter().map(|item| item.name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An operator that later adds fields to the list, or sends none because the user may list
    /// targets nowhere, still parses: the CLI then offers the current namespace alone.
    #[test]
    fn list_parses_with_missing_or_extra_fields() {
        let empty: OperatorNamespaceList = serde_json::from_str("{}").unwrap();
        assert!(empty.items.is_empty());

        let list: OperatorNamespaceList = serde_json::from_str(
            r#"{"items":[{"name":"payments","labels":{}}],"continue":"later"}"#,
        )
        .unwrap();
        assert_eq!(list.names(), vec!["payments".to_owned()]);
    }
}
