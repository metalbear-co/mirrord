//! Tera filters that turn text, and paths in particular, into regex patterns.
//!
//! A config file is rendered as text before it is parsed, so what a template prints lands inside a
//! JSON, TOML or YAML string. A Windows path printed as-is, such as the
//! `C:\Users\me\AppData\Local\Temp` of `{{ get_env(name="TEMP") }}`, breaks the file (`\U` is no
//! JSON escape), and would be no regex either. These filters print a regex that matches its input
//! literally, made of characters that no string in any of those formats needs to escape.
//!
//! - `regex_escape`: any text, such as a header value in `http_filter`.
//! - `path_pattern`: a path, in the form the `feature.fs` patterns match.
//!
//! The layer builds its own default patterns from the environment with the same functions.

use std::collections::HashMap;

use tera::{Tera, Value};

/// Why a filter couldn't turn its input into a pattern.
#[derive(Debug, thiserror::Error)]
pub enum PatternFilterError {
    #[error("expected a string, got `{0}`")]
    NotAString(Value),
    #[error("{0:?} is a control character, which a config file string can't hold")]
    ControlCharacter(char),
}

/// Makes the filters of this module available to the templates `tera` renders.
pub(crate) fn register(tera: &mut Tera) {
    tera.register_filter(
        "regex_escape",
        |value: &Value, _: &HashMap<String, Value>| filter(value, regex_escape),
    );
    tera.register_filter(
        "path_pattern",
        |value: &Value, _: &HashMap<String, Value>| filter(value, path_pattern),
    );
}

/// Runs `apply` on the string in `value`, in the shape Tera expects of a filter.
fn filter(
    value: &Value,
    apply: fn(&str) -> Result<String, PatternFilterError>,
) -> tera::Result<Value> {
    let result = match value {
        Value::String(text) => apply(text),
        other => Err(PatternFilterError::NotAString(other.clone())),
    };
    // Tera names the filter that failed around this error.
    result
        .map(Value::String)
        .map_err(|error| tera::Error::chain("invalid input", error))
}

/// Escapes `text` into a regex that matches it literally, with no backslash or quote in it.
///
/// [`regex::escape`] escapes with backslashes, which a JSON or TOML string would need doubled, so
/// the same template couldn't serve every format. Here each special character becomes a
/// character class (`.` becomes `[.]`). The few that can't sit plainly in a class become an
/// intersection of classes that holds only that character:
///
/// | Character | Class | Reads as |
/// |---|---|---|
/// | `]` | `[]]` | a `]` first in a class is literal |
/// | `[` | `[Z-[&&[^]Z]]` | `Z` to `[`, minus `]` and `Z` |
/// | `^` | `[_^&&[^_]]` | `_` or `^`, but not `_` |
/// | `\` | `[Z-^&&[^Z-[]&&[^]^]]` | `Z` to `^`, minus `Z`, `[`, `]` and `^` |
/// | `"` | `[!-#&&[^!#]]` | `!` to `#`, minus `!` and `#` |
/// | `'` | `[&-(&&[^&(]]` | `&` to `(`, minus `&` and `(` |
///
/// `http_filter` compiles its patterns with `fancy_regex`, which finds a class's end by counting
/// every `[` against every `]`, except a `]` first in the outermost class. So each class above
/// keeps that count even, or `fancy_regex` would split it where `regex` doesn't: the `[` that
/// ends `Z-[` is paired with the leading `]` of a nested class.
///
/// # Errors
///
/// A control character, such as a newline: no config string holds one unescaped.
pub fn regex_escape(text: &str) -> Result<String, PatternFilterError> {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '.' | '+' | '*' | '?' | '(' | ')' | '|' | '{' | '}' | '$' | '#' | '&' | '-' | '~' => {
                escaped.extend(['[', character, ']'])
            }
            ']' => escaped.push_str("[]]"),
            '[' => escaped.push_str("[Z-[&&[^]Z]]"),
            '^' => escaped.push_str("[_^&&[^_]]"),
            '\\' => escaped.push_str("[Z-^&&[^Z-[]&&[^]^]]"),
            '"' => escaped.push_str("[!-#&&[^!#]]"),
            '\'' => escaped.push_str("[&-(&&[^&(]]"),
            character if character.is_control() => {
                return Err(PatternFilterError::ControlCharacter(character));
            }
            character => escaped.push(character),
        }
    }
    Ok(escaped)
}

/// A regex that matches `path` the way the `feature.fs` patterns see it.
///
/// On Windows the path gets forward slashes and keeps its drive (`C:\Users\me` becomes
/// `C:/Users/me`), so it matches on that drive only. A path that exists also matches in its
/// canonical form: a short 8.3 name, common in `%TEMP%` (`C:\Users\FIRSTN~1`), also matches the
/// long name a program may open instead, and a link also matches its target. A canonical form on a
/// network share (`\\?\UNC\server\share\x`, for a mapped `H:\x`) is left out: it has no drive, so
/// it would match the path on every drive.
///
/// The result never ends in a separator. A pattern adds `/` to match what's inside a folder:
/// `"^{{ get_env(name='TEMP') | path_pattern }}/"`.
///
/// # Errors
///
/// A control character in the path, as [`regex_escape`] explains.
pub fn path_pattern(path: &str) -> Result<String, PatternFilterError> {
    let given = pattern_form(path);
    let canonical = std::fs::canonicalize(path)
        .ok()
        .and_then(|canonical| canonical.to_str().and_then(canonical_form));

    match canonical {
        // The patterns ignore case, so a canonical form that differs only in case adds nothing.
        Some(canonical) if canonical.to_lowercase() != given.to_lowercase() => Ok(format!(
            "(?:{}|{})",
            regex_escape(&given)?,
            regex_escape(&canonical)?
        )),
        _ => regex_escape(&given),
    }
}

/// A regex that matches `path` the way the `feature.fs` patterns see it, as it is spelled.
///
/// This is [`path_pattern`] without the canonical form, so it never touches the filesystem. The
/// layer builds its default patterns with it while it starts, where reading the disk is unsafe.
///
/// # Errors
///
/// A control character in the path, as [`regex_escape`] explains.
pub fn literal_path_pattern(path: &str) -> Result<String, PatternFilterError> {
    regex_escape(&pattern_form(path))
}

/// `path` in the form the `feature.fs` patterns match, without a trailing separator.
fn pattern_form(path: &str) -> String {
    #[cfg(windows)]
    let path = match str_win::path_to_unix_path(path) {
        Some(unix) => unix.with_drive().unwrap_or(unix.path),
        None => path.replace('\\', "/"),
    };

    path.trim_end_matches('/').to_owned()
}

/// `canonical`, the canonical form of a path, in the form the `feature.fs` patterns match.
///
/// [`None`] on Windows for a canonical path with no drive, such as one on a network share.
fn canonical_form(canonical: &str) -> Option<String> {
    #[cfg(windows)]
    str_win::path_to_unix_path(canonical)?.drive?;

    Some(pattern_form(canonical))
}

#[cfg(test)]
mod tests {
    use regex::{Regex, RegexBuilder};
    use rstest::rstest;

    use super::*;
    #[cfg(windows)]
    use crate::{
        LayerFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    /// An anchored, case-insensitive regex from `pattern`, as the `feature.fs` sets build them.
    fn fs_regex(pattern: &str) -> Regex {
        RegexBuilder::new(&format!("^{pattern}$"))
            .case_insensitive(true)
            .build()
            .unwrap_or_else(|error| panic!("{pattern:?} must compile: {error}"))
    }

    /// The printable ASCII characters, where every regex and config-format special lives.
    fn printable_ascii() -> impl Iterator<Item = char> {
        (b' '..=b'~').map(char::from)
    }

    /// Each printable character escapes to a regex that matches that character and no other,
    /// in either case mode. A class that also took a neighbor would loosen every pattern.
    #[test]
    fn each_character_matches_only_itself() {
        for character in printable_ascii() {
            let escaped = regex_escape(&character.to_string()).expect("printable is escapable");
            let exact = Regex::new(&format!("^{escaped}$"))
                .unwrap_or_else(|error| panic!("{escaped:?} must compile: {error}"));
            let folded = fs_regex(&escaped);

            for other in printable_ascii() {
                let same = other == character;
                assert_eq!(
                    exact.is_match(&other.to_string()),
                    same,
                    "{escaped:?} (from {character:?}) against {other:?}"
                );
                let same_folded = other.eq_ignore_ascii_case(&character);
                assert_eq!(
                    folded.is_match(&other.to_string()),
                    same_folded,
                    "{escaped:?} (from {character:?}) against {other:?}, ignoring case"
                );
            }
        }
    }

    /// `http_filter` compiles its patterns with `fancy_regex`, which parses classes itself before
    /// handing them to `regex`: each escaped character must mean the same there.
    #[test]
    fn each_character_matches_only_itself_in_an_http_filter() {
        for character in printable_ascii() {
            let escaped = regex_escape(&character.to_string()).expect("printable is escapable");
            let exact = fancy_regex::Regex::new(&format!("^{escaped}$"))
                .unwrap_or_else(|error| panic!("{escaped:?} must compile: {error}"));

            for other in printable_ascii() {
                let matched = exact
                    .is_match(&other.to_string())
                    .expect("a plain class can't fail to match");
                assert_eq!(
                    matched,
                    other == character,
                    "{escaped:?} (from {character:?}) against {other:?}"
                );
            }
        }
    }

    /// The escaped text can go inside any JSON, TOML or YAML string as-is: it holds no
    /// backslash and no quote.
    #[test]
    fn escaped_text_needs_no_escaping_in_a_config_string() {
        let every_character = printable_ascii().collect::<String>();
        let escaped = regex_escape(&every_character).expect("printable is escapable");

        assert!(
            !escaped.contains(['\\', '"', '\'']),
            "{escaped:?} holds a character a config string must escape"
        );
        assert!(fs_regex(&escaped).is_match(&every_character));
    }

    /// Text outside ASCII is plain text in a regex, so it is kept as-is.
    #[test]
    fn non_ascii_text_is_kept() {
        assert_eq!(regex_escape("Zoë/日本").expect("printable"), "Zoë/日本");
    }

    /// A control character can't be written unescaped in a config string, so it's an error
    /// rather than a pattern that breaks the file.
    #[rstest]
    #[case::newline("a\nb", '\n')]
    #[case::tab("a\tb", '\t')]
    #[case::nul("a\0b", '\0')]
    fn a_control_character_is_rejected(#[case] text: &str, #[case] control: char) {
        let error = regex_escape(text).expect_err("a control character can't be escaped");

        assert!(
            matches!(error, PatternFilterError::ControlCharacter(found) if found == control),
            "{error:?}"
        );
    }

    /// A Windows path becomes the form with the drive, so it matches on that drive only.
    #[cfg(windows)]
    #[rstest]
    #[case::backslashes(r"C:\Users\me\AppData\Local\Temp")]
    #[case::trailing_separator(r"C:\Users\me\AppData\Local\Temp\")]
    #[case::forward_slashes("C:/Users/me/AppData/Local/Temp")]
    #[case::verbatim(r"\\?\C:\Users\me\AppData\Local\Temp")]
    fn a_windows_path_matches_with_its_drive(#[case] path: &str) {
        let pattern = fs_regex(&path_pattern(path).expect("a plain path"));

        assert!(pattern.is_match("C:/Users/me/AppData/Local/Temp"));
        assert!(
            !pattern.is_match("D:/Users/me/AppData/Local/Temp"),
            "D: is another drive"
        );
        assert!(
            !pattern.is_match("/Users/me/AppData/Local/Temp"),
            "the form without a drive matches every drive"
        );
    }

    /// The characters a Windows user name can hold, which made a raw `%TEMP%` an invalid or
    /// loose regex, match only themselves.
    #[cfg(windows)]
    #[test]
    fn a_user_name_with_regex_characters_matches_literally() {
        let pattern = fs_regex(
            &path_pattern(r"C:\Users\Jo.Doe (Work) [1]\AppData\Local\Temp").expect("a plain path"),
        );

        assert!(pattern.is_match("C:/Users/Jo.Doe (Work) [1]/AppData/Local/Temp"));
        assert!(
            !pattern.is_match("C:/Users/JoXDoe Work 1/AppData/Local/Temp"),
            "`.`, `(` and `[` are literal"
        );
    }

    /// A canonical form on a network share has no drive, so it's left out rather than match the
    /// path on every drive.
    #[cfg(windows)]
    #[test]
    fn a_canonical_form_without_a_drive_is_left_out() {
        assert_eq!(canonical_form(r"\\?\UNC\server\share\Users\me"), None);
        assert_eq!(
            canonical_form(r"\\?\C:\Users\me").as_deref(),
            Some("C:/Users/me")
        );
    }

    /// A path that exists also matches in its canonical form, which programs may open instead.
    #[test]
    fn an_existing_path_matches_in_its_canonical_form_too() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        std::fs::create_dir(folder.path().join("sub")).expect("a subfolder");
        let roundabout = folder.path().join("sub").join("..");
        let roundabout = roundabout.to_str().expect("the temporary path is UTF-8");
        let canonical = std::fs::canonicalize(folder.path()).expect("the folder exists");
        let canonical = canonical.to_str().expect("the temporary path is UTF-8");

        let pattern = fs_regex(&path_pattern(roundabout).expect("a plain path"));

        assert!(
            pattern.is_match(&pattern_form(roundabout)),
            "the path as given"
        );
        assert!(
            pattern.is_match(&pattern_form(canonical)),
            "the canonical path"
        );
    }

    /// A folder from the environment, read locally through a template in each config format: the
    /// file stays valid, and the pattern matches what's in the folder, on its drive only. Printed
    /// raw, the same path is an invalid escape in JSON, TOML and double-quoted YAML.
    #[cfg(windows)]
    #[rstest]
    #[case::json(
        "json",
        r#"{ "feature": { "fs": { "local": ["^{{ get_env(name='MIRRORD_TEST_PATH_PATTERN_DIR') | path_pattern }}/"] } } }"#
    )]
    #[case::toml(
        "toml",
        "[feature.fs]\nlocal = [\"^{{ get_env(name='MIRRORD_TEST_PATH_PATTERN_DIR') | path_pattern }}/\"]\n"
    )]
    #[case::yaml(
        "yaml",
        "feature:\n  fs:\n    local:\n      - \"^{{ get_env(name='MIRRORD_TEST_PATH_PATTERN_DIR') | path_pattern }}/\"\n"
    )]
    fn a_folder_from_the_environment_becomes_a_working_fs_pattern(
        #[case] extension: &str,
        #[case] content: &str,
    ) {
        // SAFETY: no other test touches this variable, and std serializes access to the
        // environment on Windows.
        unsafe {
            std::env::set_var(
                "MIRRORD_TEST_PATH_PATTERN_DIR",
                r"C:\Users\Jo.Doe\AppData\Local\Temp",
            )
        };
        let file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .expect("a temporary config file");
        std::fs::write(file.path(), content).expect("the config file is writable");

        let mut context = ConfigContext::default();
        let config = LayerFileConfig::from_path(file.path(), &mut context)
            .expect("the rendered config parses")
            .generate_config(&mut context)
            .expect("the config resolves");

        let local = config.feature.fs.local.as_deref().expect("`local` is set");
        let patterns = regex::RegexSetBuilder::new(local)
            .case_insensitive(true)
            .build()
            .expect("the rendered pattern compiles");
        assert!(patterns.is_match("C:/Users/Jo.Doe/AppData/Local/Temp/app.log"));
        assert!(
            !patterns.is_match("C:/Users/JoXDoe/AppData/Local/Temp/app.log"),
            "the `.` in the user name is literal"
        );
        assert!(
            !patterns.is_match("D:/Users/Jo.Doe/AppData/Local/Temp/app.log"),
            "the folder is on C:"
        );
    }
}
