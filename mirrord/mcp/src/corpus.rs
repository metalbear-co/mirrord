//! The mirrord docs and skills compiled into this binary, which `mirrord mcp` answers from without
//! network access.
//!
//! They are vendored into `corpus/` from pinned commits of `metalbear-co/docs` and
//! `metalbear-co/skills` by `cargo xtask corpus`, which the release PR runs to bump the pins. The
//! tests below are the checks the content has to pass to ship, so a bad bump fails the release PR.

use std::{collections::BTreeMap, io::Read, ops::Not, sync::LazyLock};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The `corpus/` files, packed by `build.rs`.
static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/corpus.tar.gz"));

/// Every vendored file, keyed by its path in `corpus/`, e.g. `skills/mirrord-up/SKILL.md`.
pub(crate) static FILES: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    let mut archive = tar::Archive::new(GzDecoder::new(ARCHIVE));
    let entries = archive.entries().expect("build.rs packs a valid archive");
    entries
        .map(|entry| {
            let mut entry = entry.expect("build.rs packs a valid archive");
            let path = entry
                .path()
                .expect("build.rs packs valid paths")
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let mut contents = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut contents)
                .expect("build.rs packs a valid archive");
            let contents = String::from_utf8(contents).unwrap_or_else(|_| {
                panic!("`cargo xtask corpus` vendors only UTF-8 files, but `{path}` isn't")
            });
            (path, contents)
        })
        .collect()
});

/// The upstream commit a corpus is vendored from.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Pin {
    pub(crate) repo: String,
    pub(crate) commit: String,
    /// When the pin was last bumped, in RFC 3339.
    pub(crate) synced_at: String,
}

pub(crate) static DOCS_PIN: LazyLock<Pin> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../corpus/docs.pin.json")).expect("valid docs pin")
});

pub(crate) static SKILLS_PIN: LazyLock<Pin> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../corpus/skills.pin.json")).expect("valid skills pin")
});

/// A skill from the skills repo, served by `get_skill` and as an MCP prompt and resource.
pub(crate) struct Skill<'a> {
    /// From the `SKILL.md` front matter; tells the agent when to use the skill.
    pub(crate) description: String,
    /// The `SKILL.md`, front matter included.
    pub(crate) body: &'a str,
    /// The skill's other files, keyed by their path in its directory, e.g.
    /// `references/known-issues.md`.
    pub(crate) files: BTreeMap<&'a str, &'a str>,
}

impl Skill<'_> {
    /// The skill `name` as one message, for `get_skill` and the prompts: its `SKILL.md`, followed
    /// by where to get the files it bundles, which the skill refers to by path.
    pub(crate) fn text(&self, name: &str) -> String {
        let mut text = self.body.to_owned();
        if self.files.is_empty().not() {
            text.push_str(&format!(
                "\n\nThis skill bundles these files, which the `get_skill` tool returns given \
                `name: \"{name}\"` and the file's path as `file`: {}\n",
                list(self.files.keys()),
            ));
        }
        text
    }
}

/// Every skill that loads, keyed by name. Skills that don't are left out rather than failing the
/// server; the tests below keep them from shipping.
pub(crate) static SKILLS: LazyLock<BTreeMap<&'static str, Skill<'static>>> =
    LazyLock::new(|| load_skills(&FILES).0);

/// The skill `name`, for `get_skill`, the prompts and the resources alike.
pub(crate) fn skill<'s, 'a>(
    skills: &'s BTreeMap<&'a str, Skill<'a>>,
    name: &str,
) -> Result<&'s Skill<'a>, UnknownSkill> {
    skills.get(name).ok_or_else(|| UnknownSkill {
        name: name.to_owned(),
        available: list(skills.keys()),
    })
}

/// Names the available skills, so the agent can retry with one of them.
#[derive(Debug, Error)]
#[error("unknown skill `{name}`, the available skills are: {available}")]
pub struct UnknownSkill {
    name: String,
    available: String,
}

/// Names for a message, e.g. `` `mirrord-up`, `mirrord-ci` ``.
pub(crate) fn list<'a>(names: impl Iterator<Item = &'a &'a str>) -> String {
    names
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Loads the skills in `files`: every directory under `skills/` is one, and needs a `SKILL.md`
/// whose front matter names it and describes it. Returns the skills that load, and why the others
/// don't.
pub(crate) fn load_skills(
    files: &BTreeMap<String, String>,
) -> (BTreeMap<&str, Skill<'_>>, Vec<String>) {
    let mut dirs: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for (path, contents) in files {
        if let Some((skill, file)) = path
            .strip_prefix("skills/")
            .and_then(|path| path.split_once('/'))
        {
            dirs.entry(skill).or_default().insert(file, contents);
        }
    }

    let mut skills = BTreeMap::new();
    let mut issues = Vec::new();
    for (name, mut files) in dirs {
        let skill = match files.remove("SKILL.md") {
            Some(body) => parse_skill(name, body, files),
            None => Err("missing".to_owned()),
        };
        match skill {
            Ok(skill) => {
                skills.insert(name, skill);
            }
            Err(issue) => issues.push(format!("skills/{name}/SKILL.md: {issue}")),
        }
    }
    (skills, issues)
}

/// Reads the skill `name` from its `SKILL.md` (`body`) and the other files in its directory.
fn parse_skill<'a>(
    name: &str,
    body: &'a str,
    files: BTreeMap<&'a str, &'a str>,
) -> Result<Skill<'a>, String> {
    let front_matter = front_matter(body).ok_or("no front matter")?;
    let front_matter: Value = serde_saphyr::from_str(front_matter)
        .map_err(|error| format!("invalid front matter: {error}"))?;
    let field = |field| {
        front_matter
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| value.is_empty().not())
    };
    if field("name") != Some(name) {
        return Err(format!("`name` must be `{name}`"));
    }
    let description = field("description").ok_or("no `description`")?;
    Ok(Skill {
        description: description.to_owned(),
        body,
        files,
    })
}

/// The YAML between the `---` lines that open a page, if it has any.
fn front_matter(page: &str) -> Option<&str> {
    let rest = page.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    rest.get(..end)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::{FILES, front_matter, load_skills};

    /// Pages whose front matter isn't valid YAML.
    fn page_issues(files: &BTreeMap<String, String>) -> Vec<String> {
        files
            .iter()
            .filter_map(|(path, page)| {
                let error = serde_saphyr::from_str::<Value>(front_matter(page)?).err()?;
                Some(format!("{path}: invalid front matter: {error}"))
            })
            .collect()
    }

    #[test]
    fn pages_parse() {
        let issues = page_issues(&FILES);
        assert!(issues.is_empty(), "{}", issues.join("\n"));
    }

    #[test]
    fn skills_load() {
        let (_, issues) = load_skills(&FILES);
        assert!(issues.is_empty(), "{}", issues.join("\n"));
    }

    #[test]
    fn broken_corpus_is_caught() {
        let files: BTreeMap<String, String> = [
            ("docs/page.md", "---\ntitle: [unclosed\n---\n# Page\n"),
            ("skills/no-skill-md/README.md", "# Readme\n"),
            (
                "skills/misnamed/SKILL.md",
                "---\nname: other\ndescription: d\n---\n",
            ),
            (
                "skills/good/SKILL.md",
                "---\nname: good\ndescription: d\n---\n",
            ),
        ]
        .into_iter()
        .map(|(path, page)| (path.to_owned(), page.to_owned()))
        .collect();

        assert_eq!(page_issues(&files).len(), 1);
        let (skills, issues) = load_skills(&files);
        assert_eq!(skills.into_keys().collect::<Vec<_>>(), ["good"]);
        assert_eq!(
            issues,
            [
                "skills/misnamed/SKILL.md: `name` must be `misnamed`",
                "skills/no-skill-md/SKILL.md: missing",
            ]
        );
    }
}
