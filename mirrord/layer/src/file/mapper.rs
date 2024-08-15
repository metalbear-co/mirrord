use std::{borrow::Cow, collections::HashMap, path::PathBuf};

use regex::{Regex, RegexSet, RegexSetBuilder};

#[derive(Debug)]
pub struct FileRemapper {
    filter: RegexSet,
    mapping: Vec<(Regex, String)>,
}

impl FileRemapper {
    pub fn new(mapping: HashMap<String, String>) -> Self {
        let filter = RegexSetBuilder::new(mapping.keys())
            .case_insensitive(true)
            .build()
            .expect("Building path mapping regex set failed");
        let mapping = mapping
            .into_iter()
            .map(|(pattern, value)| {
                (
                    Regex::new(&pattern).expect("Building path mapping regex failed"),
                    value,
                )
            })
            .collect();

        FileRemapper { filter, mapping }
    }

    #[tracing::instrument(level = "trace", skip(self), ret)]
    fn replace_path_str<'p>(&self, mapping_index: usize, path_str: &'p str) -> Cow<'p, str> {
        let (pattern, value) = self
            .mapping
            .get(mapping_index)
            .expect("RegexSet matches returned an impossible index");

        pattern.replace(path_str, value)
    }

    // Don't instrument trace this or `change_path` because it spams a lot
    pub fn change_path_str<'p>(&self, path_str: &'p str) -> Cow<'p, str> {
        let matches = self.filter.matches(path_str);

        if let Some(index) = matches.iter().next() {
            self.replace_path_str(index, path_str)
        } else {
            Cow::Borrowed(path_str)
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

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn test_mapping() -> HashMap<String, String> {
        [
            ("/foo".to_string(), "/bar".to_string()),
            ("/(baz)".to_string(), "/tmp/mirrord-$1".to_string()),
            ("^/Users/(?<user>.+)/Library/Caches/JetBrains/(?<intellij>.+)/tomcat/(?<uuid>.+)/static/manifest.xml".to_string(), "/opt/tomcat/static/manifest.xml".to_string())
        ]
        .into()
    }

    #[rstest]
    #[case("/app/test", "/app/test")]
    #[case("/foo/test", "/bar/test")]
    #[case("/baz/test", "/tmp/mirrord-baz/test")]
    #[case("/Users/john-doe/Library/Caches/JetBrains/IntelliJIdea2023.3/tomcat/6902e44a-a069-433d-ab49-5b46477acb97/static/manifest.xml", "/opt/tomcat/static/manifest.xml")]
    #[case("/Users/john-doe/Library/Caches/JetBrains/IntelliJIdea2023.3/tomcat/6902e44a-a069-433d-ab49-5b46477acb97/static/index.html", "/Users/john-doe/Library/Caches/JetBrains/IntelliJIdea2023.3/tomcat/6902e44a-a069-433d-ab49-5b46477acb97/static/index.html")]
    fn simple_mapping(#[case] input: PathBuf, #[case] expect: PathBuf) {
        let remapper = FileRemapper::new(test_mapping());

        assert_eq!(remapper.change_path(input), expect);
    }
}
