#![feature(const_trait_impl)]
#![feature(io_error_more)]

pub mod codec;
pub mod error;
pub mod tcp;

use std::{collections::HashSet, ops::Deref};

pub use codec::*;
pub use error::*;

pub type ConnectionID = u16;
pub type Port = u16;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvVarsFilter(pub String);

impl From<EnvVarsFilter> for HashSet<String> {
    fn from(env_vars: EnvVarsFilter) -> Self {
        let mut filter = env_vars
            .split_terminator(';')
            .map(String::from)
            .collect::<HashSet<_>>();

        // TODO: These are env vars that should usually be ignored. Revisit this list if a user
        // ever asks for a way to NOT filter out these.
        filter.insert("PATH".to_string());
        filter.insert("HOME".to_string());

        filter
    }
}

impl Deref for EnvVarsFilter {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
