#![cfg(unix)]
#![feature(once_cell_try)]
#![allow(rustdoc::private_intra_doc_links)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

extern crate alloc;
extern crate core;

use std::{panic, time::Duration};

use ctor::ctor;
use mirrord_config::{LayerConfig, feature::network::incoming::IncomingMode};
use mirrord_intproxy_protocol::NewSessionRequest;
use mirrord_layer_core::{
    EXECUTABLE_ARGS, HookManager, LayerInitialization, LoadType, PROXY_CONNECTION_TIMEOUT,
    layer_pre_initialization,
};
use mirrord_layer_lib::{
    detour::DetourGuard,
    error::LayerError,
    logging::init_tracing,
    proxy_connection::{PROXY_CONNECTION, ProxyConnection},
    setup::{init_layer_setup, setup},
    trace_only::is_trace_only_mode,
};

use crate::socket::hooks::enable_socket_hooks;

mod socket;

#[ctor]
fn mirrord_layer_entry_point() {
    if cfg!(test) {
        return;
    }

    let res = panic::catch_unwind(|| match layer_pre_initialization() {
        Err(LayerError::NoProcessFound) => {}
        Err(e) => {
            eprintln!("mirrord remote layer setup failed with {e:?}");
            std::process::exit(-1)
        }
        Ok(LayerInitialization { config, load_type }) => match load_type {
            LoadType::Full => remote_start(config),
            LoadType::Skip => {}
            #[cfg(target_os = "macos")]
            LoadType::SIPOnly => {}
        },
    });

    if res.is_err() {
        eprintln!("mirrord remote layer setup panicked");
        std::process::exit(-1);
    }
}

fn remote_start(mut config: LayerConfig) {
    init_tracing();

    let proxy_connection_timeout = *PROXY_CONNECTION_TIMEOUT
        .get_or_init(|| Duration::from_secs(config.internal_proxy.socket_timeout));
    let process_info = EXECUTABLE_ARGS
        .get()
        .expect("EXECUTABLE_ARGS MUST BE SET")
        .to_process_info(&config);

    config.feature.network.incoming.mode = IncomingMode::Mirror;
    init_layer_setup(config, false);

    let mut hook_manager = HookManager::default();
    unsafe { enable_socket_hooks(&mut hook_manager) };

    let _detour_guard = DetourGuard::new();

    tracing::info!("Initializing mirrord-layer-remote!");

    if is_trace_only_mode() {
        tracing::debug!("Skipping new intproxy connection (trace only)");
        return;
    }

    unsafe {
        #[allow(static_mut_refs)]
        PROXY_CONNECTION
            .set(
                ProxyConnection::new(
                    setup().proxy_address(),
                    NewSessionRequest {
                        process_info,
                        parent_layer: None,
                    },
                    proxy_connection_timeout,
                )
                .unwrap_or_else(|_| panic!("failed to initialize proxy connection")),
            )
            .expect("setting PROXY_CONNECTION singleton")
    }
}
