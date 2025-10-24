use std::{collections::HashMap, path::Path};

use mirrord_config::feature::network::incoming::{
    IncomingConfig, IncomingMode as ConfigIncomingMode, http_filter::HttpFilterConfig,
};
use mirrord_intproxy_protocol::PortSubscription;
use mirrord_layer_lib::setup::windows::IncomingMode;
use mirrord_protocol::{ClientMessage, tcp::LayerTcpSteal};
use rstest::fixture;

/// Application enum - exactly as used in layer tests
#[derive(Clone, Copy, Debug)]
pub enum Application {
    NodeHTTP,
}

impl Application {
    pub async fn start_process_with_layer(
        &self,
        _dylib_path: &Path,
        _env: HashMap<String, String>,
        _config_path: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        // For layer-lib unit testing: Instead of starting real processes,
        // we return mocks that will be used to directly test layer-lib logic
        (
            TestProcess { child: TestChild },
            TestIntProxy {
                expected_config: _config_path.map(|p| std::fs::read_to_string(p).unwrap()),
            },
        )
    }
}

/// Test process - matches layer tests interface
pub struct TestProcess {
    pub child: TestChild,
}

/// Test child - matches layer tests interface
pub struct TestChild;

impl TestChild {
    pub async fn kill(&mut self) -> Result<(), std::io::Error> {
        Ok(())
    }
}

/// Test IntProxy - matches layer tests interface but adapted for unit testing
pub struct TestIntProxy {
    expected_config: Option<String>,
}

impl TestIntProxy {
    pub async fn recv(&mut self) -> ClientMessage {
        // In layer-lib unit tests, we simulate the expected behavior by
        // parsing the config and creating the appropriate subscription message
        if let Some(config_str) = &self.expected_config {
            let config: serde_json::Value = serde_json::from_str(config_str).unwrap();
            let http_filter = &config["feature"]["network"]["incoming"]["http_filter"];

            // Parse the http_filter using layer-lib logic
            let http_filter_config: HttpFilterConfig =
                serde_json::from_value(http_filter.clone()).unwrap();

            let mut incoming_config = IncomingConfig {
                mode: ConfigIncomingMode::Steal,
                http_filter: http_filter_config,
                ..Default::default()
            };

            // Use layer-lib logic to determine the subscription
            let incoming_mode = IncomingMode::new(&mut incoming_config);
            let subscription = incoming_mode.subscription(80);

            // Convert to the expected ClientMessage format
            match subscription {
                PortSubscription::Steal(steal_type) => {
                    ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(steal_type))
                }
                other => panic!("Unexpected subscription type: {other:?}"),
            }
        } else {
            panic!("No config provided to mock intproxy")
        }
    }
}

/// Dylib path fixture - matches layer tests interface
#[fixture]
pub fn dylib_path() -> &'static Path {
    Path::new("mock_dylib_path")
}
