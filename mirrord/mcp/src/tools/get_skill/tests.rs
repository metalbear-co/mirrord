use std::collections::BTreeMap;

use super::{GetSkillArgs, GetSkillError, get_skill};
use crate::corpus::load_skills;

fn corpus() -> BTreeMap<String, String> {
    [
        (
            "skills/a/SKILL.md",
            "---\nname: a\ndescription: d\n---\n# A\n",
        ),
        ("skills/a/references/x.md", "# X\n"),
    ]
    .into_iter()
    .map(|(path, contents)| (path.to_owned(), contents.to_owned()))
    .collect()
}

fn args(name: Option<&str>, file: Option<&str>) -> GetSkillArgs {
    GetSkillArgs {
        name: name.map(str::to_owned),
        file: file.map(str::to_owned),
    }
}

#[test]
fn skill_lists_its_files() {
    let files = corpus();
    let (skills, _) = load_skills(&files);
    let skill = get_skill(&skills, args(Some("a"), None)).unwrap();
    assert!(skill.starts_with("---\nname: a\n"), "{skill}");
    assert!(skill.contains("`references/x.md`"), "{skill}");
    assert_eq!(
        get_skill(&skills, args(Some("a"), Some("references/x.md"))).unwrap(),
        "# X\n"
    );
}

#[test]
fn unknown_file_lists_the_skill_files() {
    let files = corpus();
    let (skills, _) = load_skills(&files);
    let error = get_skill(&skills, args(Some("a"), Some("nope.md"))).unwrap_err();
    assert_eq!(
        error.to_string(),
        "skill `a` has no file `nope.md`, its files are: `references/x.md`"
    );
}

#[test]
fn file_without_name_is_refused() {
    let files = corpus();
    let (skills, _) = load_skills(&files);
    let error = get_skill(&skills, args(None, Some("references/x.md"))).unwrap_err();
    assert!(matches!(error, GetSkillError::FileWithoutName));
}
