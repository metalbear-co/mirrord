use std::{collections::HashMap, path::PathBuf};

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
    pub fn change_path(&self, path: PathBuf) -> PathBuf {
        let path_str = path.to_str().unwrap_or_default();
        let matches = self.filter.matches(path_str);

        if let Some(index) = matches.iter().next() {
            let (pattern, value) = self
                .mapping
                .get(index)
                .expect("RegexSet matches returned an imposible index");

            PathBuf::from(pattern.replace(path_str, value).as_ref())
        } else {
            path
        }
    }
}
