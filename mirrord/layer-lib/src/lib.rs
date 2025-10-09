//! Common layer functionality shared between Unix layer and Windows layer-win.

pub mod error;
pub mod macros;
pub mod proxy_connection;
pub mod setup;
pub mod socket;

pub use error::*;
pub use proxy_connection::ProxyConnection;
