#![feature(const_trait_impl)]
#![feature(io_error_more)]

pub mod codec;
pub mod dns;
pub mod error;
pub mod file;
pub mod outgoing;
pub mod tcp;

use std::{collections::HashSet, ops::Deref};

pub use codec::*;
pub use error::*;

pub type ConnectionId = u64;
pub type Port = u16;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvVars(pub String);

impl From<EnvVars> for HashSet<String> {
    fn from(env_vars: EnvVars) -> Self {
        env_vars
            .split_terminator(';')
            .map(String::from)
            .collect::<HashSet<_>>()
    }
}

impl Deref for EnvVars {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
