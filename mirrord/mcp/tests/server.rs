//! Drives [`McpServer`] through a real MCP client over an in-memory pipe, on both the
//! `initialize` handshake lifecycle and the handshake-less one that replaced it.

// Tests index into the JSON results with `value[key]`; a panic just fails the test.
#![allow(clippy::indexing_slicing)]

use mirrord_mcp::{INSTRUCTIONS, McpServer, McpTelemetry};
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, ClientConfig, GetPromptRequestParams, ProtocolVersion,
        ReadResourceRequestParams, ResourceContents,
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

fn tool_call(name: &'static str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone())
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
        .call_tool(tool_call(
            "validate_config",
            json!({
                "format": "mirrord.json",
                "content": r#"{ "feature": { "network": { "incoming": { "mode": "foo" } } } }"#,
            }),
        ))
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
        .call_tool(tool_call(
            "validate_config",
            json!({
                "format": "mirrord-up.yaml",
                "content": "services:\n  app:\n    run:\n      command: [\"true\"]\n",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        result.structured_content.unwrap(),
        json!({ "valid": true, "issues": [] })
    );
}

#[tokio::test]
async fn serves_explain_config_option() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;

    let tools = client.list_all_tools().await.unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "explain_config_option" && tool.output_schema.is_some())
    );

    let result = client
        .call_tool(tool_call(
            "explain_config_option",
            json!({ "path": "feature.network.incoming.mode" }),
        ))
        .await
        .unwrap();
    let output = result.structured_content.unwrap();
    assert_eq!(output["found"], json!(true));
    assert_eq!(output["allowed_values"], json!(["mirror", "steal", "off"]));
    assert_eq!(output["plan"], json!("oss"));
}

/// Bad calls come back as error results, and the server keeps serving afterwards.
#[tokio::test]
async fn survives_bad_calls() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;

    let result = client
        .call_tool(tool_call(
            "validate_config",
            json!({
                "format": "mirrord.toml",
                "content": "",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));

    client
        .call_tool(CallToolRequestParams::new("no_such_tool"))
        .await
        .unwrap_err();

    let result = client
        .call_tool(tool_call(
            "validate_config",
            json!({
                "format": "mirrord.json",
                "content": "{}",
            }),
        ))
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
    assert_eq!(info["version"], json!(env!("CARGO_PKG_VERSION")));
    for (corpus, repo) in [
        ("docs", "metalbear-co/docs"),
        ("skills", "metalbear-co/skills"),
    ] {
        assert_eq!(info[corpus]["repo"], json!(repo));
        assert_eq!(info[corpus]["commit"].as_str().unwrap().len(), 40);
        assert!(info[corpus]["synced_at"].is_string());
    }
}

/// Every skill `get_skill` lists is also served as a prompt and a resource, all from the same
/// `SKILL.md`.
#[tokio::test]
async fn serves_skills() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;

    let call_text = async |arguments: Value| {
        let result = client
            .call_tool(tool_call("get_skill", arguments))
            .await
            .unwrap();
        assert!(result.structured_content.is_none());
        let [content] = result.content.as_slice() else {
            panic!("expected one content block: {:?}", result.content);
        };
        (result.is_error, content.as_text().unwrap().text.clone())
    };

    let (_, listed) = call_text(json!({})).await;
    let prompts = client.list_all_prompts().await.unwrap();
    let resources = client.list_all_resources().await.unwrap();
    assert!(prompts.iter().any(|prompt| prompt.name == "mirrord-up"));
    for prompt in &prompts {
        let name = prompt.name.as_str();
        assert!(listed.contains(&format!("- `{name}`: ")), "{name}");

        let (_, skill) = call_text(json!({ "name": name })).await;
        let prompt = client
            .get_prompt(GetPromptRequestParams::new(name))
            .await
            .unwrap();
        assert_eq!(prompt.messages[0].content.as_text().unwrap().text, skill);

        let uri = format!("mirrord://skills/{name}");
        assert!(
            resources.iter().any(|resource| resource.uri == uri),
            "{name}"
        );
        let result = client
            .read_resource(ReadResourceRequestParams::new(&uri))
            .await
            .unwrap();
        let [ResourceContents::TextResourceContents { text, .. }] = result.contents.as_slice()
        else {
            panic!("unexpected contents: {:?}", result.contents);
        };
        assert!(skill.starts_with(text.as_str()), "{name}");
    }

    let (is_error, text) = call_text(json!({ "name": "no-such-skill" })).await;
    assert_eq!(is_error, Some(true));
    assert!(text.contains("`mirrord-up`"), "{text}");
}

/// What `search_docs` finds, `read_doc` and the resources serve, skills included.
#[tokio::test]
async fn serves_docs() {
    let (client, _signal) = connect(ProtocolVersion::V_2025_11_25).await;
    let resources = client.list_all_resources().await.unwrap();

    for (query, corpus) in [
        ("running without a target", "docs/"),
        ("kafka splitting known issues", "skills/"),
    ] {
        let hits = client
            .call_tool(tool_call(
                "search_docs",
                json!({ "query": query, "limit": 1 }),
            ))
            .await
            .unwrap()
            .structured_content
            .unwrap()["hits"]
            .clone();
        let [hit] = hits.as_array().unwrap().as_slice() else {
            panic!("expected one hit for `{query}`: {hits}");
        };
        assert!(hit["path"].as_str().unwrap().starts_with(corpus), "{hit}");

        let result = client
            .call_tool(tool_call("read_doc", json!({ "path": hit["path"] })))
            .await
            .unwrap();
        assert!(result.structured_content.is_none());
        let page = &result.content[0].as_text().unwrap().text;
        let title = hit["title"].as_str().unwrap();
        assert!(
            page.starts_with(&format!("Title: {title}\nSource: https://")),
            "{page}"
        );

        let uri = hit["resource_uri"].as_str().unwrap();
        if uri.starts_with("mirrord://docs/") {
            assert!(
                resources.iter().any(|resource| resource.uri == uri),
                "{uri}"
            );
        }
        let result = client
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .unwrap();
        let [ResourceContents::TextResourceContents { text, .. }] = result.contents.as_slice()
        else {
            panic!("unexpected contents: {:?}", result.contents);
        };
        assert!(page.ends_with(text.as_str()), "{uri}");
    }

    let result = client
        .call_tool(tool_call("read_doc", json!({ "path": "targetless.md" })))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let text = &result.content[0].as_text().unwrap().text;
    assert!(
        text.contains("`docs/using-mirrord/targetless.md`"),
        "{text}"
    );
}
