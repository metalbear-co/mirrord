//! Common layer functionality shared between Unix layer and Windows layer-win.

pub mod common;
pub mod dns_selector;
pub mod error;
pub mod hostname;
pub mod macros;
pub mod socket;

/// Unix-specific hostname resolver implementation.
/// Available on all platforms since remote target is always Linux.
pub mod unix;
