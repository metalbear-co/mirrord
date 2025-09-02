//! Common layer functionality shared between Unix layer and Windows layer-win.

pub mod common;
pub mod dns_selector;
pub mod hostname;
pub mod proxy_connection;
pub mod socket;

/// Unix-specific hostname resolver implementation.
/// Available on all platforms since remote target is always Linux.
pub mod unix;

pub use common::setup::LayerSetup;
pub use hostname::*;
pub use proxy_connection::ProxyConnection;
pub use unix::UnixHostnameResolver;
