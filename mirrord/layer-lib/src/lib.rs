//! Common layer functionality shared between Unix layer and Windows layer-win.

pub mod error;
pub mod file;
pub mod ide;
pub mod macros;
pub mod process;
pub mod proxy_connection;
pub mod setup;
pub mod socket;
pub mod util;

pub use error::*;
pub use proxy_connection::ProxyConnection;
