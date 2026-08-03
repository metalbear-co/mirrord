use std::collections::BTreeMap;

use fancy_regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ConnectionSource, DatabaseBranchBaseConfig};
use crate::{config::ConfigError, feature::database_branches::SingleOrVec};

/// When configuring a branch for AWS S3, set `type` to `s3`.
///
/// The source bucket name lives under `connection.params.bucket`. Its value is the name of an
/// environment variable on the target pod that contains the bucket name.
///
/// Example:
/// ```json
/// {
///   "type": "s3",
///   "connection": {
///     "params": {
///       "bucket": "S3_BUCKET"
///     }
///   },
///   "copy": { "mode": "all" }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3BranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: S3BranchCopyConfig,

    /// Additional tags to be set on the branched bucket.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_tags: BTreeMap<String, String>,
}

impl S3BranchConfig {
    pub fn verify(&self) -> Result<(), ConfigError> {
        self.base.verify()?;

        let ConnectionSource::Params(config) = &self.base.connection else {
            return Err(ConfigError::Conflict(
                "S3 branches do not support URL connections; use \
                `feature.db_branches[].connection.params` with a `bucket` key instead."
                    .to_owned(),
            ));
        };

        if config
            .params
            .extra
            .get("bucket")
            .is_none_or(|sources| sources.is_empty())
        {
            return Err(ConfigError::Conflict(
                "S3 branch requires `feature.db_branches[].connection.params.bucket` \
                to name the target environment variable that contains the bucket name."
                    .to_owned(),
            ));
        }

        let bad_pattern = match &self.copy {
            S3BranchCopyConfig::Empty | S3BranchCopyConfig::WithObjects { objects: None } => None,
            S3BranchCopyConfig::WithObjects {
                objects: Some(objects),
            } => objects.iter().find_map(|pattern| {
                let error = Regex::new(pattern).err()?;
                Some((pattern, error))
            }),
        };
        if let Some((pattern, error)) = bad_pattern {
            return Err(ConfigError::Conflict(format!(
                "S3 branch `feature.db_branches[].copy.objects` field \
                contains an invalid regular expression '{pattern}': {error}.",
            )));
        }

        Ok(())
    }
}

/// Users can choose from the following copy mode to bootstrap their S3 bucket branch.
///
/// - Empty
///
///   Creates an empty bucket.
///
/// - All
///
///   Creates a bucket with copied objects.
///   `objects` field can be used to select object to copy with regular expressions.
#[derive(Clone, Debug, Default, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum S3BranchCopyConfig {
    /// Creates an empty bucket.
    #[default]
    Empty,
    /// Creates a bucket with objects copied from the source bucket.
    WithObjects {
        /// Allows for selecting the objects to be copied.
        ///
        /// Accepts one or more regular expressions for matching the objects' names.
        /// Omit this field to copy all objects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        objects: Option<SingleOrVec<String>>,
    },
}
