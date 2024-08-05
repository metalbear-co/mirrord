use jsonschema::JSONSchema;
use serde_json::Value;
use tracing::warn;

use crate::error::DocsError;

/// Finds JSON codeblocks in the documentation.
fn find_json_blocks(docs: &Vec<String>) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current_block = String::new();

    for line in docs.iter().flat_map(|s| s.lines()) {
        let trimmed = line.trim();

        match (trimmed, in_block) {
            (s, _) if s.starts_with("```json") => {
                in_block = true;
                current_block.clear();
            }
            ("```", true) => {
                blocks.push(current_block.clone());
                in_block = false;
            }
            (_, true) => {
                current_block.push_str(line);
                current_block.push('\n');
            }
            _ => {}
        }
    }

    blocks
}

/// Validates JSON blocks in the documentation against a Json schema if possible.
/// Json schema errors are not propagated, but logged - only parsing errors are returned.
pub fn validate_json_blocks(docs: &Vec<String>, json_schema: &JSONSchema) -> Result<(), DocsError> {
    for json_block in find_json_blocks(docs) {
        let parsed_json: Value = serde_json::from_str(&json_block)
            .map_err(|error| DocsError::Json(error, json_block.clone()))?;
        let _ = json_schema.validate(&parsed_json).map_err(|errors| {
            for error in errors {
                warn!(codeblock = json_block, error = ?error, "Error validating JSON block");
            }
        });
    }
    Ok(())
}
