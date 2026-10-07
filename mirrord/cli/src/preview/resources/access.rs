//! The access check [`super::plan`] runs before a manifest replaces a running preview.
//!
//! The operator applies the same rules when it builds the pod, which is too late: `preview
//! start` has already deleted the previous session. Checking here fails `preview start` and
//! `preview diff` first.

use std::collections::BTreeSet;

use k8s_openapi::api::core::v1::PodTemplateSpec;
use mirrord_operator::preview_template::{self, TemplateAccessViolation};
use serde_json::Value;

use super::{ResourcesError, SuppliedObject};

pub(super) fn verify_template_access(
    object: &SuppliedObject,
    live: &Value,
    supplied: &Value,
    file_secrets: &BTreeSet<String>,
    target_container: &str,
) -> Result<(), ResourcesError> {
    let live = parse_template(object, live)?;
    let supplied = parse_template(object, supplied)?;
    match preview_template::verify_template_access(
        &live,
        &supplied,
        file_secrets,
        Some(target_container),
    ) {
        Ok(()) => Ok(()),
        Err(TemplateAccessViolation::ChangedField(field)) => {
            Err(ResourcesError::TemplateAccessChanged {
                object: object.display(),
                source_path: object.source.clone(),
                field,
            })
        }
        Err(TemplateAccessViolation::UnknownSecret(secret)) => {
            Err(ResourcesError::TemplateUnknownSecret {
                object: object.display(),
                source_path: object.source.clone(),
                secret,
            })
        }
    }
}

fn parse_template(
    object: &SuppliedObject,
    value: &Value,
) -> Result<PodTemplateSpec, ResourcesError> {
    serde_json::from_value(value.clone()).map_err(|source| {
        ResourcesError::TemplateAccessUnreadable {
            object: object.display(),
            source_path: object.source.clone(),
            source,
        }
    })
}
