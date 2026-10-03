//! The config schemas compiled into this binary, which every tool answers from.

use std::sync::LazyLock;

use jsonschema::{ValidationError, Validator};
use mirrord_config::LayerFileConfig;
use mirrord_up::UpConfig;
use schemars::{JsonSchema, schema_for};
use serde_json::Value;

/// A compiled schema, with the raw schema kept around to look things up in it.
pub(crate) struct Schema {
    pub(crate) raw: Value,
    pub(crate) validator: Result<Validator, ValidationError<'static>>,
}

impl Schema {
    fn new<T: JsonSchema>() -> Self {
        let raw = schema_for!(T).to_value();
        let validator = jsonschema::validator_for(&raw);
        Self { raw, validator }
    }
}

/// The schema of `mirrord.json`.
pub(crate) static LAYER_SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::new::<LayerFileConfig>);
/// The schema of `mirrord-up.yaml`.
pub(crate) static UP_SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::new::<UpConfig>);
