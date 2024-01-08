//! A LabelSelector type for a standard Kubernetes label selector.
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// The code here was adapted from k8s-openapi:
// https://github.com/Arnavion/k8s-openapi/blob/c1fc48c5c8d2f64e7da3651a4b63baf3fe0e6c27/src/v1_29/apimachinery/pkg/apis/meta/v1/label_selector.rs#L5
// https://github.com/Arnavion/k8s-openapi/blob/c1fc48c5c8d2f64e7da3651a4b63baf3fe0e6c27/src/v1_29/apimachinery/pkg/apis/meta/v1/label_selector_requirement.rs
// that code in turn was automatically generated from the Go code of Kubernetes.
// The reason we redefine those types ourselves is that in k8s-openapi the match expression operator
// is an arbitrary string, so there would be no validation in kubernetes that the operator is valid.

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum MatchExpressionOperator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}

use MatchExpressionOperator::*;

/// A label selector requirement is a selector that contains values, a key, and an operator that
/// relates the key and values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub struct LabelSelectorRequirement {
    /// key is the label key that the selector applies to.
    pub key: String,

    /// operator represents a key's relationship to a set of values. Valid operators are In, NotIn,
    /// Exists and DoesNotExist.
    pub operator: MatchExpressionOperator,

    /// values is an array of string values. If the operator is In or NotIn, the values array must
    /// be non-empty. If the operator is Exists or DoesNotExist, the values array must be empty.
    /// This array is replaced during a strategic merge patch.
    pub values: Option<Vec<String>>,
}

/// A label selector is a label query over a set of resources. The result of matchLabels and
/// matchExpressions are ANDed. An empty label selector matches all objects. A null label selector
/// matches no objects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")] // match_labels -> matchLabels in yaml.
pub struct LabelSelector {
    /// matchExpressions is a list of label selector requirements. The requirements are ANDed.
    pub match_expressions: Option<Vec<LabelSelectorRequirement>>,

    /// matchLabels is a map of {key,value} pairs. A single {key,value} in the matchLabels map is
    /// equivalent to an element of matchExpressions, whose key field is "key", the operator is
    /// "In", and the values array contains only "value". The requirements are ANDed.
    pub match_labels: Option<std::collections::BTreeMap<String, String>>,
}

/// Is the label in `labels` with the key `key` one of the values in `opt_allowed_labels`?
fn value_in_labels(
    key: &str,
    labels: &BTreeMap<String, String>,
    opt_allowed_labels: &Option<Vec<String>>,
) -> bool {
    labels
        .get(key)
        .map(|found_value| {
            opt_allowed_labels
                .as_ref()
                .map(|allowed_labels| allowed_labels.contains(found_value))
                .unwrap_or_default()
        }) // false if list of allowed labels is None.
        .unwrap_or_default() // false if there is no label with that key.
}

impl LabelSelector {
    /// Like `matches` but accepts an optional map, as present in kube resources.
    pub fn matches_optional(&self, labels: &Option<BTreeMap<String, String>>) -> bool {
        // room for optimization: if labels is none we can go over the rules and return weather they
        // are all negatives (`DoesNotExist` or `NotIn`) instead of the normal logic.
        labels
            .as_ref()
            .map(|labels| self.matches(labels))
            .unwrap_or_else(|| self.matches(&Default::default()))
    }

    /// Do all the rules of this selector apply for the given optional `labels`.
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        if let Some(label_map) = self.match_labels.as_ref() {
            for (req_key, req_value) in label_map {
                let req_fulfilled = labels
                    .get(req_key)
                    .map(|existing_value| existing_value == req_value)
                    .unwrap_or_default();
                if !req_fulfilled {
                    return false;
                }
            }
        }
        if let Some(match_reqs) = self.match_expressions.as_ref() {
            for match_req in match_reqs {
                let req_fulfilled = match match_req.operator {
                    In => value_in_labels(&match_req.key, labels, &match_req.values),
                    NotIn => !value_in_labels(&match_req.key, labels, &match_req.values),
                    Exists => labels.contains_key(&match_req.key),
                    DoesNotExist => !labels.contains_key(&match_req.key),
                };
                if !req_fulfilled {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use rstest::{fixture, rstest};

    use super::*;

    /// Convenient function so that we don't have to write `String::from` 1000 times in the tests
    /// and can just use string slices because they are shorter to write.
    fn btreemap(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        let string_items = items.iter().map(|&(key, value)| (key.into(), value.into()));
        BTreeMap::from_iter(string_items)
    }

    #[rstest]
    /// Let the tested function check if in the given mapping (day->sunday, month->next-month) `day`
    /// is either `sunday` or `friday`. Assert the function says yes (`true`).
    #[case("day", &[("day", "sunday"), ("month", "next-month")], Some(vec!["sunday", "friday"]), true)]

    /// Let the tested function check if in the given mapping (day->sunday, city->paris) `city`
    /// is either `vienna` or `oslo`. Assert the function says no (`false`).
    #[case("city", &[("day", "sunday"), ("city", "paris")], Some(vec!["vienna", "oslo"]), false)]

    /// If the allowed values are `None` then should return false.
    #[case("id", &[("level", "high"), ("id", "42")], None, false)]

    /// If the allowed values are empty then should return false.
    #[case("id", &[("level", "high"), ("id", "42")], Some(Vec::new()), false)]

    /// If the given key is not in the given map - return false
    #[case("answer", &[], Some(vec!["42", "maybe", "you-tell-me"]), false)]

    fn test_value_in_labels(
        #[case] key: &str,
        #[case] labels: &[(&str, &str)],
        #[case] opt_allowed_labels: Option<Vec<&str>>,
        #[case] result: bool,
    ) {
        let map = btreemap(labels);
        let allowed = opt_allowed_labels.map(|vals| vals.into_iter().map(From::from).collect());
        assert_eq!(value_in_labels(key, &map, &allowed), result);
    }

    /// Convenient function so that we don't have to write `String::from` 1000 times in the tests
    /// and can just use string slices because they are shorter to write.
    fn label_selector_req_with_values(
        key: &str,
        operator: &str,
        values: &[&str],
    ) -> LabelSelectorRequirement {
        LabelSelectorRequirement {
            key: key.into(),
            operator: operator.into(),
            values: Some(
                values
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .map(From::from)
                    .collect(),
            ),
        }
    }

    /// Convenient function so that we don't have to write `String::from` 1000 times in the tests
    /// and can just use string slices because they are shorter to write.
    fn label_selector_req_without_values(key: &str, operator: &str) -> LabelSelectorRequirement {
        LabelSelectorRequirement {
            key: key.into(),
            operator: operator.into(),
            values: None,
        }
    }

    #[fixture]
    fn answer_in_set() -> LabelSelectorRequirement {
        label_selector_req_with_values("answer", "In", &["42", "idk", "very-much"])
    }

    #[fixture]
    fn answer_not_in_set() -> LabelSelectorRequirement {
        label_selector_req_with_values("answer", "NotIn", &["42", "idk", "very-much"])
    }

    #[fixture]
    fn answer_exists() -> LabelSelectorRequirement {
        label_selector_req_without_values("answer", "Exists")
    }

    #[fixture]
    fn answer_does_not_exist() -> LabelSelectorRequirement {
        label_selector_req_without_values("answer", "DoesNotExist")
    }

    #[fixture]
    fn question_in_set() -> LabelSelectorRequirement {
        label_selector_req_with_values("question", "In", &["why", "how", "when"])
    }

    #[fixture]
    fn question_not_in_set() -> LabelSelectorRequirement {
        label_selector_req_with_values("question", "NotIn", &["why", "how", "when"])
    }

    #[fixture]
    fn question_exists() -> LabelSelectorRequirement {
        label_selector_req_without_values("question", "Exists")
    }

    #[fixture]
    fn question_does_not_exists() -> LabelSelectorRequirement {
        label_selector_req_without_values("question", "DoesNotExist")
    }

    #[rstest]
    fn label_selector_simple_in(answer_in_set: LabelSelectorRequirement) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_in_set]),
            match_labels: None,
        };

        let labels = btreemap(&[("answer", "42")]);
        assert!(selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_simple_in_false(answer_in_set: LabelSelectorRequirement) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_in_set]),
            match_labels: None,
        };

        let labels = btreemap(&[("answer", "yes")]);
        assert!(!selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_match_two_labels() {
        let selector = LabelSelector {
            match_expressions: None,
            match_labels: Some(btreemap(&[("answer", "42"), ("question", "who")])),
        };

        let labels = btreemap(&[("answer", "42"), ("question", "who")]);
        assert!(selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_match_only_one_of_two_labels_false() {
        let selector = LabelSelector {
            match_expressions: None,
            match_labels: Some(btreemap(&[("answer", "42"), ("question", "who")])),
        };

        let labels = btreemap(&[("answer", "1337"), ("question", "who")]);
        assert!(!selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_simple_match_label() {
        let selector = LabelSelector {
            match_expressions: None,
            match_labels: Some(btreemap(&[("answer", "42")])),
        };

        let labels = btreemap(&[("answer", "42"), ("question", "who")]);
        assert!(selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_simple_answer_exists(answer_exists: LabelSelectorRequirement) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_exists]),
            match_labels: None,
        };

        let labels = btreemap(&[("answer", "42")]);
        assert!(selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_simple_answer_exists_false(answer_exists: LabelSelectorRequirement) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_exists]),
            match_labels: None,
        };

        let labels = btreemap(&[("number", "42")]);
        assert!(!selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_simple_answer_does_not_exist(
        answer_does_not_exist: LabelSelectorRequirement,
    ) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_does_not_exist]),
            match_labels: None,
        };

        let labels = btreemap(&[("question", "why")]);
        assert!(selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_simple_answer_does_not_exist_false(
        answer_does_not_exist: LabelSelectorRequirement,
    ) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_does_not_exist]),
            match_labels: None,
        };

        let labels = btreemap(&[("answer", "42")]);
        assert!(!selector.matches(&labels))
    }

    #[rstest]
    fn label_selector_does_not_match_none_labels(answer_exists: LabelSelectorRequirement) {
        let selector = LabelSelector {
            match_expressions: Some(vec![answer_exists]),
            match_labels: None,
        };

        assert!(!selector.matches_optional(&None))
    }

    /// Empty selector should match anything
    #[rstest]
    #[case(&[("key1", "value1")])]
    #[case(&[("key1", "value1"), ("key2", "value2")])]
    fn empty_selector_matches(#[case] labels: &[(&str, &str)]) {
        let selector = LabelSelector {
            match_expressions: None,
            match_labels: None,
        };

        assert!(selector.matches_optional(&Some(btreemap(labels))))
    }

    /// Empty selector should match anything
    #[rstest]
    fn empty_selector_matches_none_labels() {
        let selector = LabelSelector {
            match_expressions: None,
            match_labels: None,
        };

        assert!(selector.matches_optional(&None))
    }
}
