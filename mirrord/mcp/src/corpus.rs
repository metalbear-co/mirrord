//! The mirrord docs and skills compiled into this binary, which `mirrord mcp` answers from without
//! network access.
//!
//! They are vendored into `corpus/` from pinned commits of `metalbear-co/docs` and
//! `metalbear-co/skills` by `cargo xtask corpus`, which the release PR runs to bump the pins. The
//! tests below are the checks the content has to pass to ship, so a bad bump fails the release PR.

use std::{collections::BTreeMap, io::Read, sync::LazyLock};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

/// The `corpus/` markdown, packed by `build.rs`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the docs and skills tools, still to come")
)]
static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/corpus.tar.gz"));

/// Every vendored file, keyed by its path in `corpus/`, e.g. `skills/mirrord-up/SKILL.md`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the docs and skills tools, still to come")
)]
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
            let mut contents = String::new();
            entry
                .read_to_string(&mut contents)
                .expect("vendored markdown is UTF-8");
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::Value;

    use super::FILES;

    /// The YAML between the `---` lines that open a page, if it has any.
    fn front_matter(page: &str) -> Option<&str> {
        let rest = page.strip_prefix("---\n")?;
        let end = rest.find("\n---\n")?;
        rest.get(..end)
    }

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

    /// Skills that can't be loaded: every directory under `skills/` is a skill, and needs a
    /// `SKILL.md` whose front matter names it and describes it.
    fn skill_issues(files: &BTreeMap<String, String>) -> Vec<String> {
        let skills: BTreeSet<&str> = files
            .keys()
            .filter_map(|path| path.strip_prefix("skills/")?.split_once('/'))
            .map(|(skill, _)| skill)
            .collect();

        skills
            .into_iter()
            .filter_map(|skill| {
                let path = format!("skills/{skill}/SKILL.md");
                let Some(page) = files.get(&path) else {
                    return Some(format!("{path}: missing"));
                };
                let Some(front_matter) = front_matter(page) else {
                    return Some(format!("{path}: no front matter"));
                };
                let front_matter: Value = serde_saphyr::from_str(front_matter).ok()?;
                let field = |name| {
                    front_matter
                        .get(name)
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                };
                if field("name") != Some(skill) {
                    return Some(format!("{path}: `name` must be `{skill}`"));
                }
                field("description")
                    .is_none()
                    .then(|| format!("{path}: no `description`"))
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
        let issues = skill_issues(&FILES);
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
        ]
        .into_iter()
        .map(|(path, page)| (path.to_owned(), page.to_owned()))
        .collect();

        assert_eq!(page_issues(&files).len(), 1);
        assert_eq!(
            skill_issues(&files),
            [
                "skills/misnamed/SKILL.md: `name` must be `misnamed`",
                "skills/no-skill-md/SKILL.md: missing",
            ]
        );
    }
}
