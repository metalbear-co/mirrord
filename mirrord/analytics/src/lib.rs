use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Possible values for analytic data
/// This is strict so we won't send sensitive data by accident.
/// (Don't add strings)
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnalyticValue {
    Bool(bool),
    Number(u32),
    Nested(Analytics),
}

/// Struct to store analytics data.
/// Example usage that would output the following json
/// ```json
/// {
///     "a": true,
///     "b": false,
///     "c": 3,
///     "extra": {
///         "d": true,
///         "e": true
///     }
/// }
/// ```
/// ```
/// use mirrord_analytics::{Analytics, CollectAnalytics};
/// let mut analytics = Analytics::default();
/// analytics.add("a", true);
/// analytics.add("b", false);
/// analytics.add("c", 3);
///
/// struct A {}
/// impl CollectAnalytics for A {
///     fn collect_analytics(&self, analytics: &mut Analytics) {
///         analytics.add("d", true);
///     }
/// }
///
/// struct B {}
/// impl CollectAnalytics for B {
///     fn collect_analytics(&self, analytics: &mut Analytics) {
///         let a = A {};
///         a.collect_analytics(analytics);
///         analytics.add("e", true);
///     }
/// }
/// let b = B {};
/// analytics.add("extra", b);
/// ```
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Analytics {
    #[serde(flatten)]
    data: HashMap<String, AnalyticValue>,
}

impl Analytics {
    pub fn add<Key: ToString, Value: Into<AnalyticValue>>(&mut self, key: Key, value: Value) {
        self.data.insert(key.to_string(), value.into());
    }
}

/// Structs that collect analytics about themselves should implement this trait
pub trait CollectAnalytics {
    ///
    fn collect_analytics(&self, analytics: &mut Analytics);
}

impl From<bool> for AnalyticValue {
    fn from(b: bool) -> Self {
        AnalyticValue::Bool(b)
    }
}

impl From<u32> for AnalyticValue {
    fn from(mode: u32) -> Self {
        AnalyticValue::Number(mode)
    }
}

impl From<Analytics> for AnalyticValue {
    fn from(analytics: Analytics) -> Self {
        AnalyticValue::Nested(analytics)
    }
}

impl<T: CollectAnalytics> From<T> for AnalyticValue {
    fn from(analytics: T) -> Self {
        let mut holder = Analytics::default();
        analytics.collect_analytics(&mut holder);
        holder.into()
    }
}

#[cfg(test)]
mod tests {
    use assert_json_diff::assert_json_eq;
    use serde_json::json;

    use super::*;
    /// this tests creates a struct that is flatten and one that is nested
    /// serializes it and verifies it's correct
    #[test]
    fn happy_flow() {
        let mut analytics = Analytics::default();
        analytics.add("a", true);
        analytics.add("b", false);
        analytics.add("c", 3);

        struct A {}
        impl CollectAnalytics for A {
            fn collect_analytics(&self, analytics: &mut Analytics) {
                analytics.add("d", true);
            }
        }

        struct B {}
        impl CollectAnalytics for B {
            fn collect_analytics(&self, analytics: &mut Analytics) {
                let a = A {};
                a.collect_analytics(analytics);
                analytics.add("e", true);
            }
        }
        let b = B {};
        analytics.add("extra", b);

        assert_json_eq!(
            analytics,
            json!({
                "a": true,
                "b": false,
                "c": 3,
                "extra": {
                    "d": true,
                    "e": true
                }
            })
        );
    }
}
