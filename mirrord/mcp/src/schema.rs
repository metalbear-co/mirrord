//! The config schemas compiled into this binary, which every tool answers from.

use std::sync::{LazyLock, OnceLock};

use jsonschema::{ValidationError, Validator};
use mirrord_config::LayerFileConfig;
use mirrord_up::UpConfig;
use schemars::{JsonSchema, schema_for};
use serde_json::Value;

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
