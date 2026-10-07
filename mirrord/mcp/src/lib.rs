//! The mirrord MCP server, served by `mirrord mcp` so AI agents can work with mirrord through
//! the [Model Context Protocol](https://modelcontextprotocol.io).
//!
//! The server speaks MCP over stdio: the agent's harness spawns `mirrord mcp` and exchanges
//! JSON-RPC messages over its stdin/stdout, so nothing else may ever be written to stdout (logs go
//! to stderr). It exposes:
//! - the tools in [`tools`], answered offline from what is compiled into this binary;
//! - the `mirrord://info` resource, describing this mirrord installation;
//! - [`INSTRUCTIONS`], which clients hand to the model.
//!
//! Usage is reported through [`telemetry`], from [`McpServer`]'s handler methods, so tools do not
//! report anything themselves.

use std::{sync::Once, time::Instant};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext},
    model::{
        CallToolRequestParams, CallToolResponse, DiscoverResult, Implementation,
        InitializeRequestParams, InitializeResult, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
        ResourceContents, ServerCapabilities, ServerConfig,
    },
    service::{RequestContext, ServerInitializeError},
    tool_handler,
};
use serde_json::json;
use thiserror::Error;

pub use crate::telemetry::McpTelemetry;
use crate::telemetry::{McpTool, ToolOutcome};

mod telemetry;
pub mod tools;

/// Handed to the model by the client, to steer it towards the tools.
pub const INSTRUCTIONS: &str = "mirrord runs local processes in the context of a Kubernetes \
cluster. Before writing or changing a mirrord config file (`mirrord.json` or `mirrord-up.yaml`), \
call `validate_config` with the complete file content and fix every issue it reports; never write \
a config that has not validated with an empty `issues` list. The `mirrord://info` resource gives \
the installed mirrord version.";

/// URI of the resource describing this mirrord installation.
const INFO_RESOURCE_URI: &str = "mirrord://info";

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP client failed to connect: {0}")]
    Initialize(#[from] Box<ServerInitializeError>),
    #[error("MCP server stopped unexpectedly: {0}")]
    Serve(#[from] tokio::task::JoinError),
}

/// Serves MCP over this process's stdin/stdout until the client disconnects.
pub async fn serve_stdio(telemetry: McpTelemetry) -> Result<(), McpError> {
    let running = McpServer::new(telemetry)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(Box::new)?;
    running.waiting().await?;
    Ok(())
}

/// The MCP request handler. One instance serves the whole connection.
pub struct McpServer {
    tool_router: ToolRouter<Self>,
    telemetry: McpTelemetry,
    /// Makes `mcp_server_started` fire once, whichever way the client starts the session.
    started: Once,
}

impl McpServer {
    pub fn new(telemetry: McpTelemetry) -> Self {
        Self {
            tool_router: Self::tool_router(),
            telemetry,
            started: Once::new(),
        }
    }

    /// Clients on protocol versions up to `2025-11-25` announce themselves in `initialize`;
    /// later ones skip the handshake and start with `server/discover`, carrying their identity in
    /// the request metadata.
    fn report_started(&self, client: Option<&Implementation>) {
        self.started
            .call_once(|| self.telemetry.server_started(client));
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("mirrord", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        self.report_started(Some(&request.client_info));
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, ErrorData> {
        self.report_started(context.client_info().as_ref());
        Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        ))
    }

    /// Routes to the tool and reports `mcp_tool_called`.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool: McpTool = request.name.parse().unwrap_or_default();
        let started_at = Instant::now();

        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, context))
            .await;

        self.telemetry
            .tool_called(tool, ToolOutcome::of(&result), started_at.elapsed());
        result
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(INFO_RESOURCE_URI, "info")
                .with_description("The installed mirrord version.")
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != INFO_RESOURCE_URI {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource `{}`", request.uri),
                None,
            ));
        }

        let info = json!({ "version": env!("CARGO_PKG_VERSION") });
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(info.to_string(), INFO_RESOURCE_URI)
                .with_mime_type("application/json"),
        ])
        .into())
    }
}
