#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![feature(core_ffi_c)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]

pub mod codec;
pub mod error;

use std::{
    collections::{HashMap, HashSet},
    lazy::SyncLazy,
    ops::Deref,
};

pub use codec::*;
pub use error::*;

static DEFAULT_IGNORE_VARS: SyncLazy<HashSet<String>> = SyncLazy::new(|| {
    let mut default_ignore = HashSet::with_capacity(4);

    // TODO: What else should be ignored?
    default_ignore.insert("PATH".to_string());
    default_ignore.insert("HOME".to_string());

    default_ignore
});

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvVars(pub String);

impl EnvVars {
    /// Ignores environment variables specified in `DEFAULT_IGNORE_VARS`.
    pub fn as_default_filtered_map(self, split_on: &str) -> HashMap<String, String> {
        self.as_filtered_map(split_on, |key, _| !DEFAULT_IGNORE_VARS.contains(key))
    }

    pub fn as_filtered_map<F>(self, split_on: &str, filter: F) -> HashMap<String, String>
    where
        F: FnMut(&String, &mut String) -> bool,
    {
        self
            // "DB=foo.db;PORT=99;HOST=;PATH=/fake"
            .split_terminator(split_on)
            // ["DB=foo.db", "PORT=99", "HOST=", "PATH=/fake"]
            .map(|key_and_value| key_and_value.split_terminator('=').collect::<Vec<_>>())
            // [["DB", "foo.db"], ["PORT", "99"], ["HOST"], ["PATH", "/fake"]]
            .filter_map(|mut keys_and_values| {
                match (keys_and_values.pop(), keys_and_values.pop()) {
                    (Some(value), Some(key)) => Some((key.to_string(), value.to_string())),
                    _ => None,
                }
            })
            // [("DB", "foo.db"), ("PORT", "99"), ("PATH", "/fake")]
            .collect::<HashMap<_, _>>()
            // [("DB", "foo.db"), ("PORT", "99")]
            .drain_filter(filter)
            .collect()
    }
}

impl Deref for EnvVars {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
