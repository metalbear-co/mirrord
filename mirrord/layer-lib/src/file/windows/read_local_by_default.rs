use std::env;

use mirrord_config::template::literal_path_pattern;
use regex::RegexSetBuilder;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
///
/// `%TEMP%` is among them, as a literal regex for the folder on its drive. A config that wants it
/// on the remote names it, e.g. `"read_only": ["^{{ get_env(name='TEMP') | path_pattern }}/"]`.
pub fn regex_set_builder() -> RegexSetBuilder {
    let mut patterns: Vec<String> = [
        r".\.dll$",
        r".\.pdb$",
        r".\.so$",
        r".\.d$",
        r".\.pyc$",
        r".\.py$",
        r".\.jar$",
        r".\.class$",
        r".\.js$",
        r".\.pth$",
        r".\.plist$",
        r".\.nls$",
        r"venv\.cfg$",
        // Python folder on Windows.
        r"^(?i)^\/Users\/[^/]+\/AppData\/Local\/Programs\/Python",
        r"^(?i)^\/windows\/system32",
        r"^(?i)^\/Program Files",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    // As spelled in the variable: the layer is starting, and must not read the disk to resolve it.
    if let Some(temp) = env::var("TEMP")
        .ok()
        .and_then(|temp| literal_path_pattern(&temp).ok())
    {
        patterns.push(format!("^{temp}/"));
    }

    RegexSetBuilder::new(patterns)
}
