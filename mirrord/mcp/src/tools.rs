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

use crate::{McpServer, corpus::SKILLS};

pub mod explain_config_option;
pub mod get_skill;
pub mod read_doc;
pub mod search_docs;
pub mod validate_config;

use explain_config_option::{ExplainConfigOptionArgs, ExplainConfigOptionOutput};
use get_skill::GetSkillArgs;
use read_doc::{ReadDocArgs, ReadDocOutput};
use search_docs::{SearchDocsArgs, SearchDocsOutput};
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

    /// Describe one config option from the schema of the installed mirrord version.
    #[tool(
        name = "explain_config_option",
        description = "Explain one option of a mirrord config file (`mirrord.json` or \
        `mirrord-up.yaml`), given its dotted path such as `feature.network.incoming.mode` or \
        `services.api.http_filter.header_filter`, from the schema of the installed mirrord \
        version. Returns what the option does, the JSON types and values it accepts, its default \
        and the mirrord plan it needs, each where the schema records it. For a \
        `mirrord-up.yaml` option, also returns the `mirrord.json` option it sets. An unknown path \
        returns the closest known paths instead. Call this before setting an option you are not \
        sure about.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn explain_config_option(
        &self,
        Parameters(args): Parameters<ExplainConfigOptionArgs>,
    ) -> Result<Json<ExplainConfigOptionOutput>, CallToolResult> {
        explain_config_option::explain_config_option(args)
            .map(Json)
            .map_err(|error| CallToolResult::error(vec![ContentBlock::text(error.to_string())]))
    }

    /// List the vendored skills, or get one of them or a file bundled with it.
    #[tool(
        name = "get_skill",
        description = "Get the mirrord skills: step-by-step guides for mirrord tasks such as \
        configuring a session, running several services with `mirrord up`, installing the \
        Operator, splitting queues or branching databases. Call it without arguments to list \
        every skill with what it is for, then with the `name` of the skill that matches the task \
        to get its `SKILL.md`, and follow it. Pass `file` as well to get one of the files the \
        skill bundles, which are listed after its `SKILL.md`.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn get_skill(
        &self,
        Parameters(args): Parameters<GetSkillArgs>,
    ) -> Result<String, CallToolResult> {
        get_skill::get_skill(&SKILLS, args)
            .map_err(|error| CallToolResult::error(vec![ContentBlock::text(error.to_string())]))
    }

    /// Keyword search over the vendored docs and skills.
    #[tool(
        name = "search_docs",
        description = "Search the mirrord docs and skills shipped with the installed mirrord \
        version by keywords, e.g. `steal http filter` or `db branching postgres`. Returns the best \
        matching pages first, each with its title, the `path` to read it with `read_doc`, its \
        resource URI and the line that best matches. `limit` is the number of hits, 5 by default \
        and at most 20. Search the docs before answering a question about mirrord from memory.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn search_docs(&self, Parameters(args): Parameters<SearchDocsArgs>) -> Json<SearchDocsOutput> {
        Json(search_docs::search_docs(args))
    }

    /// Serve one page of the vendored docs and skills, or list them.
    #[tool(
        name = "read_doc",
        description = "Read one page of the mirrord docs or skills shipped with the installed \
        mirrord version, by the `path` `search_docs` returns. Returns the page's full markdown, \
        its title and the URL it is published at. Without a `path`, lists every page. An unknown \
        path returns the closest known paths.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn read_doc(
        &self,
        Parameters(args): Parameters<ReadDocArgs>,
    ) -> Result<Json<ReadDocOutput>, CallToolResult> {
        read_doc::read_doc(args)
            .map(Json)
            .map_err(|error| CallToolResult::error(vec![ContentBlock::text(error.to_string())]))
    }
}
