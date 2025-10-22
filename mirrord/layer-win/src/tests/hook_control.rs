//! Tests for configuration-based hook control

#[cfg(test)]
mod tests {
    use mirrord_config::feature::{
        fs::FsModeConfig,
        network::{NetworkConfig, incoming::IncomingMode},
    };

    #[test]
    fn test_fs_hook_config_trait() {
        use mirrord_layer_lib::setup::windows::FsHookConfig;

        // Test Local mode (hooks disabled)
        assert!(!FsModeConfig::Local.is_active());
        assert!(!FsModeConfig::Local.should_enable_read_hooks());
        assert!(!FsModeConfig::Local.should_enable_write_hooks());
        assert!(!FsModeConfig::Local.should_enable_metadata_hooks());

        // Test Read mode (read hooks enabled)
        assert!(FsModeConfig::Read.is_active());
        assert!(FsModeConfig::Read.should_enable_read_hooks());
        assert!(!FsModeConfig::Read.should_enable_write_hooks());
        assert!(FsModeConfig::Read.should_enable_metadata_hooks());

        // Test Write mode (all hooks enabled)
        assert!(FsModeConfig::Write.is_active());
        assert!(FsModeConfig::Write.should_enable_read_hooks());
        assert!(FsModeConfig::Write.should_enable_write_hooks());
        assert!(FsModeConfig::Write.should_enable_metadata_hooks());
    }

    #[test]
    fn test_network_hook_config_trait() {
        use mirrord_layer_lib::setup::windows::NetworkHookConfig;

        let mut config = NetworkConfig {
            incoming: Default::default(),
            outgoing: Default::default(),
            dns: Default::default(),
            ipv6: Default::default(),
        };

        // Configure all network features as disabled
        config.incoming.mode = IncomingMode::Off;
        config.outgoing.tcp = false;
        config.outgoing.udp = false;

        assert!(!config.requires_incoming_hooks());
        assert!(!config.requires_outgoing_hooks());
        assert!(!config.requires_tcp_hooks());
        assert!(!config.requires_udp_hooks());

        // Enable incoming mirror mode
        config.incoming.mode = IncomingMode::Mirror;
        assert!(config.requires_incoming_hooks());

        // Enable TCP outgoing
        config.outgoing.tcp = true;
        assert!(config.requires_outgoing_hooks());
        assert!(config.requires_tcp_hooks());

        // Enable UDP outgoing
        config.outgoing.udp = true;
        assert!(config.requires_udp_hooks());
    }

    #[test]
    fn test_hook_enablement_logic() {
        // This test verifies the core logic of hook enablement
        // without requiring full LayerConfig construction

        use mirrord_layer_lib::setup::windows::{FsHookConfig, NetworkHookConfig};

        // File system hook logic
        let local_mode = FsModeConfig::Local;
        let read_mode = FsModeConfig::Read;
        let write_mode = FsModeConfig::Write;

        // Only Local mode should disable hooks
        assert!(!local_mode.is_active());
        assert!(read_mode.is_active());
        assert!(write_mode.is_active());

        // Read hooks should be enabled for Read and Write modes
        assert!(!local_mode.should_enable_read_hooks());
        assert!(read_mode.should_enable_read_hooks());
        assert!(write_mode.should_enable_read_hooks());

        // Write hooks should only be enabled for Write mode
        assert!(!local_mode.should_enable_write_hooks());
        assert!(!read_mode.should_enable_write_hooks());
        assert!(write_mode.should_enable_write_hooks());

        // Network hook logic
        let mut network_all_off = NetworkConfig {
            incoming: Default::default(),
            outgoing: Default::default(),
            dns: Default::default(),
            ipv6: Default::default(),
        };
        network_all_off.incoming.mode = IncomingMode::Off;
        network_all_off.outgoing.tcp = false;
        network_all_off.outgoing.udp = false;

        let mut network_tcp_only = network_all_off.clone();
        network_tcp_only.outgoing.tcp = true;

        let mut network_incoming_only = network_all_off.clone();
        network_incoming_only.incoming.mode = IncomingMode::Mirror;

        // All off should require no hooks
        assert!(!network_all_off.requires_incoming_hooks());
        assert!(!network_all_off.requires_outgoing_hooks());

        // TCP only should require outgoing hooks
        assert!(!network_tcp_only.requires_incoming_hooks());
        assert!(network_tcp_only.requires_outgoing_hooks());
        assert!(network_tcp_only.requires_tcp_hooks());
        assert!(!network_tcp_only.requires_udp_hooks());

        // Incoming only should require incoming hooks
        assert!(network_incoming_only.requires_incoming_hooks());
        assert!(!network_incoming_only.requires_outgoing_hooks());
    }
}
