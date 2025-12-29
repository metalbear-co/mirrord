//! Tests for configuration-based hook control

#[cfg(test)]
mod tests {
    use mirrord_config::feature::network::{NetworkConfig, incoming::IncomingMode};

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

        use mirrord_layer_lib::setup::windows::NetworkHookConfig;

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
