use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DatabaseBranchBaseConfig, RelationConfig, SubsetLimitsConfig, SubsetSeedConfig};
use crate::config::ConfigError;

/// When configuring a branch for MongoDB, set `type` to `mongodb`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MongodbBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MongodbBranchCopyConfig,
}

/// Users can choose from the following copy mode to bootstrap their MongoDB branch database:
///
/// - Empty
///
///   Creates an empty database. If the source DB connection options are found from the chosen
///   target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
///   operator looks for the `name` field from the branch DB config object. This option is useful
///   for users that run DB migrations themselves before starting the application.
///
/// - All
///
///   Copies both schema and data of all collections. Supports optional collection filters
///   to copy only specific collections or filter documents within collections.
///
/// - Subset
///
///   Copies the documents related to the seed documents selected in `collections`,
///   discovered by following the `relations` declared in the config (MongoDB has no
///   foreign keys, so relations must be declared explicitly), in both directions
///   (referenced parents and referencing children). Seeds use structured `conditions`,
///   e.g.
///   `{ "mode": "subset", "collections": { "users": { "conditions": [{ "column": "_id", "op": "eq",
/// "value": "u5" }] } }, "relations": [{ "from": { "collection": "orders", "field": "userId" },
/// "to": { "collection": "users", "field": "_id" } }] }`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum MongodbBranchCopyConfig {
    Empty {
        collections: Option<BTreeMap<String, MongodbBranchCollectionCopyConfig>>,
    },

    All {
        /// Optional collection filters. If not specified, all collections are copied.
        /// If specified, only the listed collections are copied with their optional filters.
        collections: Option<BTreeMap<String, MongodbBranchCollectionCopyConfig>>,
    },

    Subset {
        /// Seed collections with structured conditions selecting the starting documents.
        collections: BTreeMap<String, SubsetSeedConfig>,
        /// Relationships between collections to follow when discovering related
        /// documents. Required: MongoDB has no declared foreign keys to detect.
        relations: Vec<RelationConfig>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limits: Option<SubsetLimitsConfig>,
    },
}

impl Default for MongodbBranchCopyConfig {
    fn default() -> Self {
        MongodbBranchCopyConfig::Empty {
            collections: Default::default(),
        }
    }
}

impl MongodbBranchCopyConfig {
    pub fn verify(&self) -> Result<(), ConfigError> {
        let Self::Subset {
            collections,
            relations,
            ..
        } = self
        else {
            return Ok(());
        };

        super::verify_subset_seeds(collections, "feature.db_branches[].copy.collections")?;
        if relations.is_empty() {
            return Err(ConfigError::InvalidValue {
                name: "feature.db_branches[].copy.relations".into(),
                provided: "[]".to_owned(),
                error: "subset copy mode on MongoDB requires `relations`: MongoDB has no \
                        declared foreign keys, so the relationships to follow must be listed \
                        in the config"
                    .into(),
            });
        }
        Ok(())
    }
}

pub type MongodbBranchCollectionCopyConfig = super::BranchItemCopyConfig;
