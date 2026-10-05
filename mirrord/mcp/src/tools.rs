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

pub mod generate_config;
pub mod validate_config;

use generate_config::{GenerateConfigArgs, GenerateConfigOutput};
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

    /// Write a mirrord config from structured options.
    #[tool(
        name = "generate_config",
        description = "Generate a mirrord config file from structured options: a `mirrord.json` \
        from `config`, or a `mirrord-up.yaml` from `common` and `services`. Only the options given \
        are written, the same options always give the same content, and the content has passed \
        `validate_config`; options that make an invalid config are refused with the reasons. \
        `requires_operator` lists the generated options that need the mirrord Operator. Write \
        `content` to the file as is. Option values are rendered as templates, like the file \
        itself, so `{{ key }}` works and a literal `{{` or `{%` has to be escaped as a template \
        expression with a backtick-quoted string: {{ `{{` }}.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn generate_config(
        &self,
        Parameters(args): Parameters<GenerateConfigArgs>,
    ) -> Result<Json<GenerateConfigOutput>, CallToolResult> {
        generate_config::generate_config(args)
            .map(Json)
            .map_err(|error| CallToolResult::error(vec![ContentBlock::text(error.to_string())]))
    }
}
