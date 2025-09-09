pub mod proxy_connection;
pub mod setup;

use std::{net::SocketAddr, sync::OnceLock, time::Duration};

use mirrord_config::{MIRRORD_LAYER_INTPROXY_ADDR, util::read_resolved_config};
use mirrord_intproxy_protocol::ProcessInfo;
use proxy_connection::{PROXY_CONNECTION, ProxyConnection};

use crate::{
    common::setup::LayerSetup,
    error::{LayerError, LayerResult},
};

/// Global layer setup instance, shared across the layer
static LAYER_SETUP: OnceLock<LayerSetup> = OnceLock::new();

/// Initialize proxy connection with mirrord agent
///
/// This function handles all the setup required to establish a connection with the mirrord agent:
/// - Reads the proxy address from environment variables
/// - Reads and processes the mirrord configuration
/// - Creates the layer setup
/// - Establishes the proxy connection
/// - Stores the setup for later use
pub fn initialize_proxy_connection(process_info: ProcessInfo) -> LayerResult<()> {
    // Read proxy address from environment
    let address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
        .map_err(LayerError::MissingEnvIntProxyAddr)?
        .parse::<SocketAddr>()
        .map_err(LayerError::MalformedIntProxyAddr)?;

    // Read and process mirrord configuration
    let config = read_resolved_config().map_err(LayerError::Config)?;

    // Create layer setup
    let setup = LayerSetup::new(config);

    // Set default timeout
    let timeout = Duration::from_secs(30);

    // Set up session request
    let session = mirrord_intproxy_protocol::NewSessionRequest {
        parent_layer: None,
        process_info,
    };

    // Establish proxy connection
    let new_connection = ProxyConnection::new(address, session, timeout)
        .map_err(LayerError::ProxyConnectionFailed)?;

    // Store the proxy connection
    PROXY_CONNECTION
        .set(new_connection)
        .map_err(|_| LayerError::GlobalAlreadyInitialized("ProxyConnection"))?;

    // Store the setup in the global static
    LAYER_SETUP
        .set(setup)
        .map_err(|_| LayerError::GlobalAlreadyInitialized("LayerSetup"))?;

    Ok(())
}

/// Get access to the layer setup
///
/// # Panics
///
/// Panics if the layer setup has not been initialized yet.
pub fn layer_setup() -> &'static LayerSetup {
    LAYER_SETUP.get().expect("Layer setup not initialized")
}
