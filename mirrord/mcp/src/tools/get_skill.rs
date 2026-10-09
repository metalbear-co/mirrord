//! The `get_skill` tool: serves the skills vendored from `metalbear-co/skills`, so an agent with
//! only `mirrord mcp` configured can find and follow the skill for its task without installing the
//! skills separately. The skills are listed from the [corpus], never by hand.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct GetSkillOutput {
    /// Every skill, when no `name` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    skills: Option<Vec<SkillSummary>>,
    /// The requested `SKILL.md` or bundled file.
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    /// The files bundled with the requested skill, which `file` accepts.
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SkillSummary {
    name: String,
    /// What the skill is for, and when to use it.
    description: String,
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

/// Answers from `skills`, which the server passes as [`SKILLS`](crate::corpus::SKILLS).
pub(crate) fn get_skill(
    skills: &BTreeMap<&str, Skill<'_>>,
    args: GetSkillArgs,
) -> Result<GetSkillOutput, GetSkillError> {
    let GetSkillArgs { name, file } = args;
    let Some(name) = name else {
        if file.is_some() {
            return Err(GetSkillError::FileWithoutName);
        }
        let skills = skills
            .iter()
            .map(|(name, skill)| SkillSummary {
                name: (*name).to_owned(),
                description: skill.description.clone(),
            })
            .collect();
        return Ok(GetSkillOutput {
            skills: Some(skills),
            ..Default::default()
        });
    };

    let skill = corpus::skill(skills, &name)?;
    let content = match file.as_deref() {
        None | Some("SKILL.md") => skill.body,
        Some(file) => skill
            .files
            .get(file)
            .copied()
            .ok_or_else(|| GetSkillError::UnknownFile {
                name,
                file: file.to_owned(),
                available: list(skill.files.keys()),
            })?,
    };
    Ok(GetSkillOutput {
        content: Some(content.to_owned()),
        files: Some(skill.files.keys().map(|file| (*file).to_owned()).collect()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests;
