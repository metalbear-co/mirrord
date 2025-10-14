//! Setup and configuration for mirrord layers
//!
//! This module provides common components for platform specific LayerSetup
//! In the future, this will hold LayerSetup when it's fully cross-platform
pub mod windows;

use std::sync::OnceLock;

use mirrord_config::LayerConfig;

/// Holds LayerConfig for platform specific LayerSetup for layer-lib accessibilty
/// This is used to access the configuration from anywhere in the layer-lib
/// Initialized in LayerSetup
pub static CONFIG: OnceLock<LayerConfig> = OnceLock::new();

pub fn layer_config() -> &'static LayerConfig {
    CONFIG.get().expect("Layer config not initialized")
}
