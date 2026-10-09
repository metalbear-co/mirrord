use regex::RegexSetBuilder;

/// This is the list of path patterns that are read locally by default in all fs modes. If you want
/// to read or write in the cluster a path covered by those patterns, you need to include it in a
/// pattern in the `feature.fs.read_only` or `feature.fs.read_write` configuration field,
/// respectively.
///
/// Folders that depend on the user's environment, like `%TEMP%`, aren't here: a config reads them
/// locally with a template, e.g. `"local": ["^{{ get_env(name='TEMP') | path_pattern }}/"]`.
pub fn regex_set_builder() -> RegexSetBuilder {
    RegexSetBuilder::new([
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
    ])
}
