use mirrord_config::{
    LayerConfig,
    feature::{fs::FsModeConfig, network::incoming::IncomingMode},
};

/// Environment variable name for enabling trace-only mode (no agent connection)
pub const TRACE_ONLY_ENV: &str = "MIRRORD_LAYER_TRACE_ONLY";

/// Check if trace-only mode is enabled via environment variable
pub fn is_trace_only_mode() -> bool {
    std::env::var(TRACE_ONLY_ENV)
        .unwrap_or_default()
        .parse()
        .unwrap_or(false)
}

/// Modify configuration for trace-only mode (disable agent-dependent features)
pub fn modify_config_for_trace_only(config: &mut LayerConfig) {
    tracing::info!("Trace-only mode enabled - disabling agent-dependent features");

    config.feature.fs.mode = FsModeConfig::Local;
    config.feature.network.dns.enabled = false;
    config.feature.network.incoming.mode = IncomingMode::Off;
    // Note: outgoing and env configs don't have simple enabled flags
    // They will operate locally when no agent connection exists

    tracing::debug!("Configuration modified for trace-only mode");
}
