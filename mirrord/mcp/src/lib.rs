//! The mirrord MCP server, served by `mirrord mcp` so AI agents can work with mirrord through
//! the [Model Context Protocol](https://modelcontextprotocol.io).
//!
//! The server speaks MCP over stdio: the agent's harness spawns `mirrord mcp` and exchanges
//! JSON-RPC messages over its stdin/stdout, so nothing else may ever be written to stdout (logs go
//! to stderr). It exposes:
//! - the tools in [`tools`], answered offline from what is compiled into this binary;
//! - the `mirrord://info` resource, describing this mirrord installation and the [`corpus`] it
//!   ships;
//! - every vendored skill, as a prompt and as a `mirrord://skills/<name>` resource;
//! - every docs page, as a `mirrord://docs/<path>` resource;
//! - [`INSTRUCTIONS`], which clients hand to the model.
//!
//! Usage is reported through [`telemetry`], from [`McpServer`]'s handler methods, so tools do not
//! report anything themselves.

use std::{sync::Once, time::Instant};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext},
    model::{
        CallToolRequestParams, CallToolResponse, DiscoverResult, GetPromptRequestParams,
        GetPromptResponse, GetPromptResult, Implementation, InitializeRequestParams,
        InitializeResult, ListPromptsResult, ListResourcesResult, PaginatedRequestParams, Prompt,
        PromptMessage, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
        Resource, ResourceContents, Role, ServerCapabilities, ServerConfig,
    },
    service::{RequestContext, ServerInitializeError},
    tool_handler,
};
use serde_json::json;
use thiserror::Error;

pub use crate::telemetry::McpTelemetry;
use crate::{
    corpus::{PAGES, SKILLS},
    telemetry::{McpTool, ToolOutcome},
};

mod corpus;
mod schema;
mod telemetry;
pub mod tools;

/// Handed to the model by the client, to steer it towards the tools.
pub const INSTRUCTIONS: &str = "mirrord runs local processes in the context of a Kubernetes \
cluster.

Before starting any mirrord task, call `get_skill` without arguments to list the mirrord skills, \
then get the skill that matches the task and follow it.

Every mirrord config you generate or change, whether a `mirrord.json` or a `mirrord-up.yaml`, \
must be checked with `validate_config` before it is written: pass the complete file content, fix \
every issue it reports and validate again, until `issues` is empty. Never write a config that has \
not validated, and don't rely on your own knowledge of the config format, which may not match the \
installed mirrord version. For `mirrord.json` and `mirrord-up.yaml`, `validate_config` replaces \
the checks a skill describes against bundled schemas or with `mirrord verify-config`; keep the \
skill's other checks, such as those of the Kubernetes resources it generates. To learn \
what an option does, which values it takes or which mirrord plan it needs, call \
`explain_config_option` with its path instead of guessing. The \
`mirrord://info` resource gives the installed mirrord version.

To answer a question about mirrord, search the docs with `search_docs` and read the pages it finds \
with `read_doc`, rather than answering from memory.

When mirrord fails, validate every config involved with `validate_config` before proposing a \
fix, and propose one fix at a time.

Clusters are usually shared with other developers:
- Default to `mirror` for incoming traffic. Use `steal` only when your process must be the one \
responding, and on a shared cluster steal with an HTTP filter so you only take your own requests.
- Target staging or development clusters, never production.
- Nothing `mirrord exec` or `mirrord up` runs is deployed to the cluster, and their sessions end \
when they exit. Database branches a session creates stay until their TTL runs out, and preview \
environments are deployed and stay until stopped or their TTL runs out.";

/// URI of the resource describing this mirrord installation.
const INFO_RESOURCE_URI: &str = "mirrord://info";

/// Prefix of the URIs of the skill resources, followed by the skill's name.
const SKILL_RESOURCE_URI_PREFIX: &str = "mirrord://skills/";

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
                .enable_prompts()
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
        let info = Resource::new(INFO_RESOURCE_URI, "info")
            .with_description(
                "The installed mirrord version, and the docs and skills commits it ships.",
            )
            .with_mime_type("application/json");
        let skills = SKILLS.iter().map(|(name, skill)| {
            Resource::new(format!("{SKILL_RESOURCE_URI_PREFIX}{name}"), *name)
                .with_description(skill.description.clone())
                .with_mime_type("text/markdown")
        });
        let docs = PAGES
            .iter()
            .filter(|(path, _)| path.starts_with("docs/"))
            .map(|(path, page)| {
                Resource::new(&page.resource_uri, *path)
                    .with_title(&page.title)
                    .with_mime_type("text/markdown")
            });
        Ok(ListResourcesResult::with_all_items(
            std::iter::once(info).chain(skills).chain(docs).collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if let Some(page) = PAGES.values().find(|page| page.resource_uri == request.uri) {
            let mime_type = match request.uri.rsplit_once('.') {
                Some((_, "json")) => "application/json",
                Some((_, "yaml")) => "application/yaml",
                _ => "text/markdown",
            };
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(page.body, request.uri).with_mime_type(mime_type),
            ])
            .into());
        }
        if let Some(name) = request.uri.strip_prefix(SKILL_RESOURCE_URI_PREFIX) {
            let name = name.split('/').next().unwrap_or(name);
            corpus::skill(&SKILLS, name)
                .map_err(|error| ErrorData::resource_not_found(error.to_string(), None))?;
        }
        if request.uri != INFO_RESOURCE_URI {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource `{}`", request.uri),
                None,
            ));
        }

        let info = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "docs": &*corpus::DOCS_PIN,
            "skills": &*corpus::SKILLS_PIN,
        });
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(info.to_string(), INFO_RESOURCE_URI)
                .with_mime_type("application/json"),
        ])
        .into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(
            SKILLS
                .iter()
                .map(|(name, skill)| Prompt::new(*name, Some(&skill.description), None))
                .collect(),
        ))
    }

    /// Serves a skill's `SKILL.md`, followed by where to get the files it bundles: a prompt is
    /// one message, and the skill refers to those files by path.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let skill = corpus::skill(&SKILLS, &request.name)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            skill.text(&request.name),
        )])
        .with_description(skill.description.clone())
        .into())
    }
}
