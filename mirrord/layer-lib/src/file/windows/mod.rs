pub mod not_found_by_default;
pub mod read_local_by_default;
pub mod read_remote_by_default;

use regex::{RegexSet, RegexSetBuilder};

/// List of files that mirrord should use locally, as they probably exist only in the local user
/// machine, or are system configuration files (that could break the process if we used the remote
/// version).
///
/// You most likely do **NOT** want to include any of these, but if have a reason to do so, then
/// setting any of the overrides - `MIRRORD_FILE_X_PATTERN` allows you to override this list.
pub fn generate_local_set() -> RegexSet {
    read_local_by_default::regex_set_builder()
        .case_insensitive(true)
        .build()
        .expect("Building local path regex set failed")
}

/// List of files that mirrord should use remotely read only
pub fn generate_remote_ro_set() -> RegexSet {
    let patterns = read_remote_by_default::PATHS;
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building remote readonly path regex set failed")
}

pub fn generate_not_found_set() -> RegexSet {
    let patterns = not_found_by_default::PATHS;
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("Building remote not_found path regex set failed")
}