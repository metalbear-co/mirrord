#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![feature(core_ffi_c)]

pub mod codec;
pub mod error;
pub mod tcp;

pub use codec::*;
pub use error::*;

pub type ConnectionID = u16;
pub type Port = u16;
