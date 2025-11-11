use regex::RegexSetBuilder;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
pub fn regex_set_builder() -> RegexSetBuilder {
    let patterns: Vec<String> = [
        // Python folder on Windows.
        r"^/Users/[^/]+/AppData/Local/Programs/Python/"
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    RegexSetBuilder::new(patterns)
}


