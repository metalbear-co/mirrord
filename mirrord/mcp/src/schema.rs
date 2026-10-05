//! The config schemas compiled into this binary, which every tool answers from, and the walk over
//! them that every tool shares.

use std::{
    collections::HashSet,
    sync::{LazyLock, OnceLock},
};

use jsonschema::{ValidationError, Validator};
use mirrord_config::LayerFileConfig;
use mirrord_up::UpConfig;
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use serde_json::Value;

use crate::tools::explain_config_option::Plan;

/// A config schema, and the validator compiled from it.
pub(crate) struct Schema {
    pub(crate) raw: Value,
    /// Compiled on first use: `explain_config_option` only reads `raw`, and `validate_config` only
    /// needs the validator for a config that doesn't deserialize.
    validator: OnceLock<Result<Validator, ValidationError<'static>>>,
}

impl Schema {
    fn new<T: JsonSchema>() -> Self {
        Self {
            raw: schema_for!(T).to_value(),
            validator: OnceLock::new(),
        }
    }

    pub(crate) fn validator(&self) -> &Result<Validator, ValidationError<'static>> {
        self.validator
            .get_or_init(|| jsonschema::validator_for(&self.raw))
    }
}

/// The schema of `mirrord.json`.
pub(crate) static LAYER_SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::new::<LayerFileConfig>);
/// The schema of `mirrord-up.yaml`.
pub(crate) static UP_SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::new::<UpConfig>);

/// Schema annotation naming the [`Plan`] an option needs. Options without it inherit it from the
/// closest annotated option above them.
pub(crate) const PLAN_ANNOTATION: &str = "x-mirrord-plan";

/// A schema reached while resolving a path.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Node<'s> {
    pub(crate) schema: &'s Value,
    /// The plan from the schema's own [`PLAN_ANNOTATION`], or else the one inherited from the
    /// schemas that led to it.
    pub(crate) plan: Option<Plan>,
}

impl<'s> Node<'s> {
    pub(crate) fn root(schema: &'s Value) -> Self {
        Self { schema, plan: None }.child(schema)
    }

    /// `schema`, reached from `self`.
    pub(crate) fn child(self, schema: &'s Value) -> Self {
        let plan = schema
            .get(PLAN_ANNOTATION)
            .and_then(|plan| Plan::deserialize(plan).ok())
            .or(self.plan);
        Self { schema, plan }
    }
}

/// The schemas some value may be checked against.
pub(crate) struct Expanded<'s> {
    /// The given schemas and every schema they refer to or offer as an alternative, outermost
    /// first, so the docs of an option come before those of its type.
    pub(crate) nodes: Vec<Node<'s>>,
    /// The `$ref`s that were followed, to stop at recursive types when walking the schema.
    pub(crate) refs: HashSet<&'s str>,
}

/// Follows `$ref`, `anyOf`, `oneOf` and `allOf` from `nodes`.
pub(crate) fn expand<'s>(
    root: &'s Value,
    nodes: impl IntoIterator<Item = Node<'s>>,
) -> Expanded<'s> {
    fn visit<'s>(root: &'s Value, node: Node<'s>, expanded: &mut Expanded<'s>) {
        expanded.nodes.push(node);

        if let Some(reference) = node.schema.get("$ref").and_then(Value::as_str)
            && expanded.refs.insert(reference)
            && let Some(target) = reference
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer))
        {
            visit(root, node.child(target), expanded);
        }

        for keyword in ["anyOf", "oneOf", "allOf"] {
            for branch in node
                .schema
                .get(keyword)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                visit(root, node.child(branch), expanded);
            }
        }
    }

    let mut expanded = Expanded {
        nodes: Vec::new(),
        refs: HashSet::new(),
    };
    for node in nodes {
        visit(root, node, &mut expanded);
    }
    expanded
}
