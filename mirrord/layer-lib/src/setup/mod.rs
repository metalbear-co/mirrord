//! Setup and configuration for mirrord layers
//!
//! This module provides common components for platform specific LayerSetup
//! In the future, this will hold LayerSetup when it's fully cross-platform
#[cfg(target_os = "windows")]
pub mod windows;

use std::net::SocketAddr;
#[cfg(target_os = "windows")]
use std::sync::OnceLock;

use mirrord_config::LayerConfig;

use crate::error::{LayerError, LayerResult};
#[cfg(target_os = "windows")]
use crate::setup::windows::LayerSetup;

#[cfg(target_os = "windows")]
static SETUP: OnceLock<LayerSetup> = OnceLock::new();

#[cfg(target_os = "windows")]
pub fn layer_setup() -> &'static LayerSetup {
    SETUP.get().expect("LayerSetup is not initialized")
}

#[cfg(target_os = "windows")]
pub fn init_setup(config: LayerConfig, proxy_address: SocketAddr) -> LayerResult<()> {
    let state = LayerSetup::new(config, proxy_address);
    SETUP
        .set(state)
        .map_err(|_| LayerError::GlobalAlreadyInitialized("Layer setup already initialized"))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn layer_setup() -> ! {
    panic!("LayerSetup is only available on Windows")
}

#[cfg(not(target_os = "windows"))]
pub fn init_setup(_config: LayerConfig, _proxy_address: SocketAddr) -> LayerResult<()> {
    Err(LayerError::GlobalAlreadyInitialized(
        "LayerSetup is only supported on Windows",
    ))
}
