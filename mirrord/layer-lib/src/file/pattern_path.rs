//! The forms of a file path that the `feature.fs` patterns are matched against.

use regex::RegexSet;

/// A file path in every form the `feature.fs` patterns (`read_only`, `local`, `mapping`, ...) are
/// matched against.
///
/// On Unix a path has one form. On Windows the layer matches a path both without its drive
/// (`/Repos/app.json`) and with it (`C:/Repos/app.json`). So a pattern that names a drive applies
/// to that drive only, and one that doesn't matches the path on every drive, which is how
/// configs written before patterns could name a drive expect it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternPath<'a> {
    /// The path without a drive. This is also the path the agent opens.
    pub path: &'a str,
    /// The path with its drive (`C:/Repos/app.json`), for a Windows path on a drive.
    pub with_drive: Option<&'a str>,
}

impl PatternPath<'_> {
    /// Whether a pattern in `set` matches either form of the path.
    pub fn is_match(&self, set: &RegexSet) -> bool {
        set.is_match(self.path)
            || self
                .with_drive
                .is_some_and(|with_drive| set.is_match(with_drive))
    }
}

impl<'a> From<&'a str> for PatternPath<'a> {
    fn from(path: &'a str) -> Self {
        Self {
            path,
            with_drive: None,
        }
    }
}

impl<'a> From<&'a String> for PatternPath<'a> {
    fn from(path: &'a String) -> Self {
        Self::from(path.as_str())
    }
}
