//! Common layer functionality shared between Unix layer and Windows layer-win.
#![feature(return_type_notation)]

extern crate alloc;

pub mod debugger_ports;
pub mod detour;
pub mod error;
pub mod file;
pub mod logging;
pub mod macros;
pub mod mutex;
pub mod process;
pub mod proxy_connection;
pub mod setup;
pub mod socket;
pub mod trace_only;

pub use error::*;
pub use proxy_connection::ProxyConnection;
