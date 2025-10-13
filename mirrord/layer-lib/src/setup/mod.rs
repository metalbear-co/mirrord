//! Setup and configuration for mirrord layers
//!
//! This module provides common components for platform specific LayerSetup
//! In the future, this will hold LayerSetup when it's fully cross-platform
#[cfg(target_os = "windows")]
pub mod windows;

use std::{net::SocketAddr, sync::OnceLock};

use mirrord_config::LayerConfig;

use crate::{
    error::{LayerError, LayerResult},
    setup::windows::LayerSetup,
};

static SETUP: OnceLock<LayerSetup> = OnceLock::new();

pub fn layer_setup() -> &'static LayerSetup {
    SETUP.get().expect("LayerSetup is not initialized")
}

pub fn init_setup(config: LayerConfig, proxy_address: SocketAddr) -> LayerResult<()> {
    let state = LayerSetup::new(config, proxy_address);
    SETUP
        .set(state)
        .map_err(|_| LayerError::GlobalAlreadyInitialized("Layer setup already initialized"))?;
    Ok(())
}
