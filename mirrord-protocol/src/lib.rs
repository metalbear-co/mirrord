#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![feature(core_ffi_c)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]

pub mod codec;
pub mod error;

use std::{collections::HashSet, ops::Deref};

pub use codec::*;
pub use error::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvVarsFilter(pub String);

impl From<EnvVarsFilter> for HashSet<String> {
    fn from(env_vars: EnvVarsFilter) -> Self {
        env_vars.split_terminator(";").map(String::from).collect()
    }
}

impl Deref for EnvVarsFilter {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
