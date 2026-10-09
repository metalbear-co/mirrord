use std::{borrow::Cow, collections::HashMap, path::PathBuf};

use regex::{Regex, RegexSet, RegexSetBuilder};

use super::pattern_path::PatternPath;

/// Rewrites file paths with the `feature.fs.mapping` patterns, before the fs filter decides where
/// a path is opened.
#[derive(Debug)]
pub struct FileRemapper {
    filter: RegexSet,
    mapping: Vec<(Regex, String)>,
}

impl FileRemapper {
    /// # Errors
    ///
    /// A mapping pattern that is not a valid regex.
    pub fn try_new(mapping: HashMap<String, String>) -> Result<Self, regex::Error> {
        let filter = RegexSetBuilder::new(mapping.keys())
            .case_insensitive(true)
            .build()?;
        let mapping = mapping
            .into_iter()
            .map(|(pattern, value)| Ok((Regex::new(&pattern)?, value)))
            .collect::<Result<_, regex::Error>>()?;

        Ok(FileRemapper { filter, mapping })
    }

    #[mirrord_layer_macro::instrument(level = "trace", skip(self), ret)]
    fn replace_path_str<'p>(&self, mapping_index: usize, path_str: &'p str) -> Cow<'p, str> {
        let (pattern, value) = self
            .mapping
            .get(mapping_index)
            .expect("RegexSet matches returned an impossible index");

        pattern.replace(path_str, value)
    }

    // Don't instrument trace this or `change_path` because it spams a lot
    pub fn change_path_str<'p>(&self, path_str: &'p str) -> Cow<'p, str> {
        self.change_pattern_path(path_str.into())
    }

    /// Rewrites `path` with the first mapping that matches any of its forms (see
    /// [`PatternPath`]), returning the path the agent opens.
    ///
    /// A mapping that matches the path without its drive replaces in that form, so a value that
    /// rewrites only part of the path can't pull the drive into it. Only a mapping that needs the
    /// drive to match (`^C:/Repos/`) replaces in the form with the drive. The result never keeps
    /// a drive, because the remote has none.
    // Don't instrument trace this either, for the same reason.
    pub fn change_pattern_path<'p>(&self, path: PatternPath<'p>) -> Cow<'p, str> {
        let first_without_drive = self.filter.matches(path.path).iter().next();
        let first_with_drive = path.with_drive.and_then(|with_drive| {
            let index = self.filter.matches(with_drive).iter().next()?;
            Some((index, with_drive))
        });

        match (first_without_drive, first_with_drive) {
            // A mapping that matches with the drive and comes first can't match without it,
            // or it would be `first_without_drive`.
            (without, Some((index, with_drive)))
                if without.is_none_or(|without| index < without) =>
            {
                match self.replace_path_str(index, with_drive) {
                    Cow::Borrowed(replaced) => Cow::Borrowed(strip_drive(replaced)),
                    Cow::Owned(replaced) => Cow::Owned(strip_drive(&replaced).to_owned()),
                }
            }
            (Some(index), _) => self.replace_path_str(index, path.path),
            (None, _) => Cow::Borrowed(path.path),
        }
    }

    // Don't instrument trace this or `change_path_str` because it spams a lot
    pub fn change_path(&self, path: PathBuf) -> PathBuf {
        let path_str = path.to_str().unwrap_or_default();

        match self.change_path_str(path_str) {
            Cow::Borrowed(borrowed_path) if borrowed_path == path_str => path,
            updated_path => PathBuf::from(updated_path.as_ref()),
        }
    }
}

/// `replaced` without a leading drive (`C:/srv/app.json` becomes `/srv/app.json`), so the agent
/// never gets one.
///
/// Only a mapping that needs the drive to match replaces in the form with the drive, and its value
/// may still carry one, as in `^C:/Repos/(.*)` mapped to `C:/srv/$1`.
fn strip_drive(replaced: &str) -> &str {
    match replaced.as_bytes() {
        [letter, b':', rest @ ..]
            if letter.is_ascii_alphabetic() && rest.first().is_none_or(|next| *next == b'/') =>
        {
            replaced
                .get(2..)
                .filter(|rest| !rest.is_empty())
                .unwrap_or("/")
        }
        _ => replaced,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn test_mapping() -> HashMap<String, String> {
        [
            ("/foo".to_owned(), "/bar".to_owned()),
            ("/(baz)".to_owned(), "/tmp/mirrord-$1".to_owned()),
            ("^/Users/(?<user>.+)/Library/Caches/JetBrains/(?<intellij>.+)/tomcat/(?<uuid>.+)/static/manifest.xml".to_owned(), "/opt/tomcat/static/manifest.xml".to_owned())
        ]
        .into()
    }

    #[rstest]
    #[case("/app/test", "/app/test")]
    #[case("/foo/test", "/bar/test")]
    #[case("/baz/test", "/tmp/mirrord-baz/test")]
    #[case(
        "/Users/john-doe/Library/Caches/JetBrains/IntelliJIdea2023.3/tomcat/6902e44a-a069-433d-ab49-5b46477acb97/static/manifest.xml",
        "/opt/tomcat/static/manifest.xml"
    )]
    #[case(
        "/Users/john-doe/Library/Caches/JetBrains/IntelliJIdea2023.3/tomcat/6902e44a-a069-433d-ab49-5b46477acb97/static/index.html",
        "/Users/john-doe/Library/Caches/JetBrains/IntelliJIdea2023.3/tomcat/6902e44a-a069-433d-ab49-5b46477acb97/static/index.html"
    )]
    fn simple_mapping(#[case] input: PathBuf, #[case] expect: PathBuf) {
        let remapper = FileRemapper::try_new(test_mapping()).unwrap();

        assert_eq!(remapper.change_path(input), expect);
    }

    fn remapper(mapping: &[(&str, &str)]) -> FileRemapper {
        FileRemapper::try_new(
            mapping
                .iter()
                .map(|(pattern, value)| ((*pattern).to_owned(), (*value).to_owned()))
                .collect(),
        )
        .expect("the test mappings are valid regexes")
    }

    /// `C:\Repos\app\appsettings.json` as the Windows layer passes it to the mapper.
    const ON_C: PatternPath = PatternPath {
        path: "/Repos/app/appsettings.json",
        with_drive: Some("C:/Repos/app/appsettings.json"),
    };

    /// `D:\Repos\app\appsettings.json`, the same path on another drive.
    const ON_D: PatternPath = PatternPath {
        path: "/Repos/app/appsettings.json",
        with_drive: Some("D:/Repos/app/appsettings.json"),
    };

    /// A mapping without a drive applies on every drive.
    #[test]
    fn a_mapping_without_a_drive_applies_on_every_drive() {
        let remapper = remapper(&[("^/Repos/app/(.*)$", "/app/$1")]);

        assert_eq!(remapper.change_pattern_path(ON_C), "/app/appsettings.json");
        assert_eq!(remapper.change_pattern_path(ON_D), "/app/appsettings.json");
    }

    /// A mapping that names a drive rewrites paths on that drive only.
    #[test]
    fn a_mapping_with_a_drive_applies_to_that_drive_only() {
        let remapper = remapper(&[("^C:/Repos/app/(.*)$", "/app/$1")]);

        assert_eq!(remapper.change_pattern_path(ON_C), "/app/appsettings.json");
        assert_eq!(
            remapper.change_pattern_path(ON_D),
            "/Repos/app/appsettings.json",
            "a path on D: is left alone"
        );
    }

    /// The mapping a customer wrote with the full Windows path, in forward slashes, as they'd copy
    /// it from Explorer.
    #[test]
    fn a_full_windows_path_in_forward_slashes_maps() {
        let remapper = remapper(&[("C:/Repos/app/appsettings.json", "/app/appsettings.json")]);

        assert_eq!(remapper.change_pattern_path(ON_C), "/app/appsettings.json");
    }

    /// A pattern that matches without the drive replaces in that form, so a value that rewrites
    /// only part of the path doesn't drag the drive along to the agent.
    #[test]
    fn a_partial_replacement_never_keeps_the_drive() {
        let remapper = remapper(&[(r"appsettings\.json$", "settings.json")]);

        assert_eq!(
            remapper.change_pattern_path(ON_C),
            "/Repos/app/settings.json"
        );
    }

    /// A drive left in the result of a drive mapping is dropped, since the remote has no drives.
    #[rstest]
    #[case::drive_in_the_value("^C:/Repos/app/(.*)$", "C:/srv/$1", "/srv/appsettings.json")]
    #[case::drive_kept_by_a_capture("^(C:)/Repos/app/(.*)$", "$1/srv/$2", "/srv/appsettings.json")]
    #[case::only_the_drive_is_left("^(C:)/Repos/app/appsettings.json$", "$1", "/")]
    fn a_drive_left_by_the_replacement_is_dropped(
        #[case] pattern: &str,
        #[case] value: &str,
        #[case] expect: &str,
    ) {
        let remapper = remapper(&[(pattern, value)]);

        assert_eq!(remapper.change_pattern_path(ON_C), expect);
    }

    /// Text that only looks like the start of a drive is left alone.
    #[rstest]
    #[case::no_separator_after_the_colon("C:srv")]
    #[case::not_a_letter("1:/srv")]
    #[case::unix_path("/srv/C:/x")]
    fn only_a_real_drive_is_stripped(#[case] replaced: &str) {
        assert_eq!(strip_drive(replaced), replaced);
    }
}
