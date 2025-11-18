use std::env;

use regex::RegexSetBuilder;
use str_win::path_to_unix_path;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
pub fn regex_set_builder() -> RegexSetBuilder {
    let mut patterns: Vec<String> = [
        r".\.dll$",
        r".\.so$",
        r".\.d$",
        r".\.pyc$",
        r".\.py$",
        r".\.jar$",
        r".\.class$",
        r".\.js$",
        r".\.pth$",
        r".\.plist$",
        r"venv\.cfg$",
        // Python folder on Windows.
        r"^/Users/[^/]+/AppData/Local/Programs/Python/",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    // Add %TEMP% to read-local-by-default.
    if let Ok(Some(mut temp)) = env::var("TEMP").map(path_to_unix_path) {
        if !temp.ends_with("/") {
            temp.push('/');
        }

        patterns.push(temp);
    }

    RegexSetBuilder::new(patterns)
}
