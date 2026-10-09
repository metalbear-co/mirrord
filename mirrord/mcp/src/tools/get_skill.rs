//! The `get_skill` tool: serves the skills vendored from `metalbear-co/skills`, so an agent with
//! only `mirrord mcp` configured can find and follow the skill for its task without installing the
//! skills separately. The skills are listed from the [corpus], never by hand.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::corpus::{self, Skill, UnknownSkill, list};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSkillArgs {
    /// The skill to get, e.g. `mirrord-up`. Leave out to list every skill.
    name: Option<String>,
    /// A file bundled with the skill, by its path in the skill's directory, e.g.
    /// `references/known-issues.md`. Leave out, or pass `SKILL.md`, to get the skill's `SKILL.md`.
    file: Option<String>,
}

#[derive(Debug, Error)]
pub enum GetSkillError {
    #[error(transparent)]
    UnknownSkill(#[from] UnknownSkill),
    #[error("skill `{name}` has no file `{file}`, its files are: {available}")]
    UnknownFile {
        name: String,
        file: String,
        available: String,
    },
    #[error("`file` needs the `name` of the skill it belongs to")]
    FileWithoutName,
}

/// Answers from `skills`, which the server passes as [`SKILLS`](crate::corpus::SKILLS), in
/// markdown. Plain text rather than structured output, which an MCP result also carries serialized
/// as text, so a skill file would go out twice and escaped.
pub(crate) fn get_skill(
    skills: &BTreeMap<&str, Skill<'_>>,
    args: GetSkillArgs,
) -> Result<String, GetSkillError> {
    let GetSkillArgs { name, file } = args;
    let Some(name) = name else {
        if file.is_some() {
            return Err(GetSkillError::FileWithoutName);
        }
        return Ok(skills
            .iter()
            .map(|(name, skill)| format!("- `{name}`: {}\n", skill.description))
            .collect());
    };

    let skill = corpus::skill(skills, &name)?;
    match file.as_deref() {
        None | Some("SKILL.md") => Ok(skill.text(&name)),
        Some(file) => skill
            .files
            .get(file)
            .map(|contents| (*contents).to_owned())
            .ok_or_else(|| GetSkillError::UnknownFile {
                name,
                file: file.to_owned(),
                available: list(skill.files.keys()),
            }),
    }
}

#[cfg(test)]
mod tests;
