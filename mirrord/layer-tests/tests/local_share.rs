#![cfg(target_os = "linux")]

mod common;

use std::io::Write;

pub use common::*;
use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
};

/// The first listed port, which the link opens, survives `verify-config --resolved`, saving its
/// output as a config file and loading it again, and the internal handoff to the layer.
#[tokio::test]
async fn local_share_resolved_config_keeps_first_port() {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let mut file = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    file.write_all(
        b"target = 'pod/mock-target'\nshare = true\n[feature.network.incoming]\nmode = 'steal'\nports = [9000, 8080]\n",
    )
    .unwrap();
    let mut context = ConfigContext::default().strict_env(true);
    let config = LayerFileConfig::from_path(file.path(), &mut context)
        .unwrap()
        .generate_config(&mut context)
        .unwrap();
    assert!(config.share);
    assert_eq!(
        config.feature.network.incoming.first_configured_port,
        Some(9000)
    );
    let output =
        mirrord_command::resolve_tokio_command(std::env::var("MIRRORD_TESTS_USE_BINARY").unwrap())
            .env("TMPDIR", tmpdir)
            .args(["verify-config", "--resolved"])
            .arg(file.path())
            .output()
            .await
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let verified: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        verified.get("type").and_then(serde_json::Value::as_str),
        Some("Success"),
        "{verified}"
    );
    let resolved = verified.get("resolved_config").unwrap();
    assert_eq!(
        resolved.pointer("/feature/network/incoming/ports"),
        Some(&serde_json::json!([9000, 8080])),
        "{verified}"
    );
    let mut saved = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    serde_json::to_writer(&mut saved, resolved).unwrap();
    let reloaded = LayerFileConfig::from_path(saved.path(), &mut context)
        .unwrap()
        .generate_config(&mut context)
        .unwrap();
    assert!(reloaded.share);
    assert_eq!(
        reloaded.feature.network.incoming.first_configured_port,
        Some(9000)
    );
    assert_eq!(
        reloaded.feature.network.incoming.ports,
        Some([9000, 8080].into_iter().collect())
    );
    // Internal handoffs must retain generated metadata rather than project file settings.
    let handed_off = LayerConfig::decode(&reloaded.encode().unwrap()).unwrap();
    assert_eq!(
        handed_off.feature.network.incoming.first_configured_port,
        Some(9000)
    );
    assert_eq!(handed_off.key, reloaded.key);
}
