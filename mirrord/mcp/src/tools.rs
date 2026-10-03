//! The tools served by `mirrord mcp`.
//!
//! Adding a tool takes a `#[tool]` method in the router below and a variant in
//! [`McpTool`](crate::telemetry::McpTool); telemetry for every call is handled by
//! [`McpServer::call_tool`](rmcp::ServerHandler::call_tool).

use rmcp::{
    Json,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    tool, tool_router,
};

use crate::McpServer;

pub mod validate_config;

use validate_config::{ValidateConfigArgs, ValidateConfigOutput};

#[tool_router(vis = "pub(crate)")]
impl McpServer {
    /// Check a mirrord config against the schema of the installed mirrord version.
    #[tool(
        name = "validate_config",
        description = "Validate the content of a mirrord config file (`mirrord.json` or \
        `mirrord-up.yaml`) against the schema of the installed mirrord version. Returns every \
        issue found, each with the JSON pointer of the offending value, a message and, where the \
        schema enumerates them, the allowed values. An empty `issues` list means the config is \
        valid. Call this before writing or changing a mirrord config file.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn validate_config(
        &self,
        Parameters(args): Parameters<ValidateConfigArgs>,
    ) -> Result<Json<ValidateConfigOutput>, CallToolResult> {
        validate_config::validate_config(args)
            .map(Json)
            .map_err(|error| CallToolResult::error(vec![ContentBlock::text(error.to_string())]))
    }
}
