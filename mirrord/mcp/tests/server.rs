//! Drives [`McpServer`] through a real MCP client over an in-memory pipe, on both the
//! `initialize` handshake lifecycle and the handshake-less one that replaced it.

// Tests index into the JSON results with `value[key]`; a panic just fails the test.
#![allow(clippy::indexing_slicing)]

use mirrord_mcp::{INSTRUCTIONS, McpServer, McpTelemetry};
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, ClientConfig, ProtocolVersion, ReadResourceRequestParams,
        ResourceContents,
    },
    service::RunningService,
};
use rstest::rstest;
use serde_json::{Value, json};
use uuid::Uuid;

struct Client {
    protocol_version: ProtocolVersion,
}

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        let mut info = ClientConfig::default();
        info.protocol_version = self.protocol_version.clone();
        info.client_info.name = "mirrord-mcp-test".to_owned();
        info
    }
}

/// Serves a server with telemetry off and connects a client to it. The drain signal is returned
/// so the watch it hands out stays alive for the duration of the test.
async fn connect(
    protocol_version: ProtocolVersion,
) -> (RunningService<RoleClient, Client>, drain::Signal) {
    let (signal, watch) = drain::channel();
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);

    let server = McpServer::new(McpTelemetry {
        enabled: false,
        machine_id: Uuid::new_v4(),
        watch,
    });
    tokio::spawn(async move {
        server
            .serve(server_transport)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });

    let client = Client { protocol_version }
        .serve(client_transport)
        .await
        .unwrap();
    (client, signal)
}

fn validate_config_call(arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new("validate_config")
        .with_arguments(arguments.as_object().unwrap().clone())
}

#[rstest]
#[case::initialize(ProtocolVersion::V_2025_11_25)]
#[case::discover(ProtocolVersion::V_2026_07_28)]
#[tokio::test]
async fn serves_validate_config(#[case] protocol_version: ProtocolVersion) {
    let (client, _signal) = connect(protocol_version).await;

    let server_info = client.peer_info().unwrap();
    assert_eq!(server_info.server_info.as_ref().unwrap().name, "mirrord");
    assert_eq!(server_info.instructions.as_deref(), Some(INSTRUCTIONS));

    let tools = client.list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "validate_config")
        .unwrap();
    assert!(tool.output_schema.is_some());

    let result = client
        .call_tool(validate_config_call(json!({
            "format": "mirrord.json",
            "content": r#"{ "feature": { "network": { "incoming": { "mode": "foo" } } } }"#,
        })))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let output = result.structured_content.unwrap();
    assert_eq!(output["valid"], json!(false));
    assert_eq!(
        output["issues"][0]["path"],
        json!("/feature/network/incoming/mode")
    );

    let result = client
        .call_tool(validate_config_call(json!({
            "format": "mirrord-up.yaml",
            "content": "services:\n  app:\n    run:\n      command: [\"true\"]\n",
        })))
        .await
        .unwrap();
    assert_eq!(
        result.structured_content.unwrap(),
        json!({ "valid": true, "issues": [] })
    );
}

/// A generated config comes back as structured content that `validate_config` accepts.
#[tokio::test]
async fn serves_generate_config() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;

    let tools = client.list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "generate_config")
        .unwrap();
    assert!(tool.output_schema.is_some());

    let result = client
        .call_tool(
            CallToolRequestParams::new("generate_config").with_arguments(
                json!({
                    "format": "mirrord-up.yaml",
                    "services": [{
                        "name": "api",
                        "run": { "command": ["npm", "run", "dev"] },
                        "config": { "copy_target": { "scale_down": true } },
                    }],
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let output = result.structured_content.unwrap();
    assert_eq!(
        output["requires_operator"],
        json!(["/services/api/config_patch/feature/copy_target"])
    );

    let result = client
        .call_tool(validate_config_call(json!({
            "format": output["format"],
            "content": output["content"],
        })))
        .await
        .unwrap();
    assert_eq!(
        result.structured_content.unwrap(),
        json!({ "valid": true, "issues": [] })
    );
}

/// Bad calls come back as error results, and the server keeps serving afterwards.
#[tokio::test]
async fn survives_bad_calls() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;

    let result = client
        .call_tool(validate_config_call(json!({
            "format": "mirrord.toml",
            "content": "",
        })))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));

    client
        .call_tool(CallToolRequestParams::new("no_such_tool"))
        .await
        .unwrap_err();

    let result = client
        .call_tool(validate_config_call(json!({
            "format": "mirrord.json",
            "content": "{}",
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["valid"], json!(true));
}

#[tokio::test]
async fn serves_info_resource() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;

    let resources = client.list_all_resources().await.unwrap();
    assert!(
        resources
            .iter()
            .any(|resource| resource.uri == "mirrord://info")
    );

    let result = client
        .read_resource(ReadResourceRequestParams::new("mirrord://info"))
        .await
        .unwrap();
    let [ResourceContents::TextResourceContents { text, .. }] = result.contents.as_slice() else {
        panic!("unexpected contents: {:?}", result.contents);
    };
    let info: Value = serde_json::from_str(text).unwrap();
    assert_eq!(info, json!({ "version": env!("CARGO_PKG_VERSION") }));
}
