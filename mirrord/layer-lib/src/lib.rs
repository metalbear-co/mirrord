//! Common layer functionality shared between Unix layer and Windows layer-win.

pub mod hostname;
pub mod proxy_connection;

/// Unix-specific hostname resolver implementation.
/// Available on all platforms since remote target is always Linux.
pub mod unix;

pub use hostname::*;
pub use proxy_connection::ProxyConnection;
pub use unix::UnixHostnameResolver;
