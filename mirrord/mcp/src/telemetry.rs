//! Usage reporting for `mirrord mcp`, sent through the CLI's regular analytics reporter so the
//! events carry the same `machine_id` and AI agent detection as every other mirrord event.
//!
//! [`AnalyticValue`](mirrord_analytics::AnalyticValue) deliberately has no string variant, so
//! everything identifying (tool names, client names, outcomes) is sent as a `u32` from the
//! `#[repr(u32)]` enums below. Tool arguments and config content are never reported.

use std::time::Duration;

use mirrord_analytics::{AnalyticsReporter, ReportTarget, Reporter};
use rmcp::{
    ErrorData,
    model::{CallToolResponse, ErrorCode, Implementation},
};
use strum_macros::EnumString;
use uuid::Uuid;

/// What the MCP server needs to report usage. Built by the CLI, which owns the machine id, the
/// telemetry setting and the drain that flushes pending reports on exit.
#[derive(Debug, Clone)]
pub struct McpTelemetry {
    /// `false` when the user turned telemetry off; nothing is sent then.
    pub enabled: bool,
    pub machine_id: Uuid,
    pub watch: drain::Watch,
}

impl McpTelemetry {
    /// Reports `mcp_server_started`. `client` is the implementation the client announced, which is
    /// missing only when a client skips announcing itself.
    pub(crate) fn server_started(&self, client: Option<&Implementation>) {
        let mut reporter = self.reporter(ReportTarget::McpServerStarted);
        let analytics = reporter.get_mut();
        analytics.add("mcp_transport", McpTransport::Stdio as u32);

        let Some(client) = client else {
            return;
        };
        analytics.add("mcp_client", McpClient::from_name(&client.name) as u32);
        if let Ok(version) = semver::Version::parse(&client.version) {
            for (key, part) in [
                ("mcp_client_version_major", version.major),
                ("mcp_client_version_minor", version.minor),
                ("mcp_client_version_patch", version.patch),
            ] {
                analytics.add(key, u32::try_from(part).unwrap_or(u32::MAX));
            }
        }
    }

    /// Reports `mcp_tool_called` for one finished `tools/call`.
    pub(crate) fn tool_called(&self, tool: McpTool, outcome: ToolOutcome, duration: Duration) {
        let mut reporter = self.reporter(ReportTarget::McpToolCalled);
        let analytics = reporter.get_mut();
        analytics.add("mcp_tool", tool as u32);
        analytics.add("mcp_tool_outcome", outcome as u32);
        analytics.add(
            "duration_ms",
            u32::try_from(duration.as_millis()).unwrap_or(u32::MAX),
        );
    }

    /// The report is sent when the returned reporter is dropped.
    fn reporter(&self, target: ReportTarget) -> AnalyticsReporter {
        AnalyticsReporter::for_mcp_event(target, self.enabled, self.watch.clone(), self.machine_id)
    }
}

/// The tools served by `mirrord mcp`. Every tool registered in the router needs a variant here,
/// parsed from its MCP name (the variant name in snake case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, EnumString)]
#[strum(serialize_all = "snake_case")]
#[repr(u32)]
pub(crate) enum McpTool {
    /// A name the server does not serve.
    #[default]
    #[strum(disabled)]
    Unknown = 0,
    ValidateConfig = 1,
    ExplainConfigOption = 2,
    GetSkill = 3,
    SearchDocs = 4,
    ReadDoc = 5,
}

/// How a `tools/call` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum ToolOutcome {
    Success = 0,
    /// The tool ran and returned an error result (`isError: true`), which includes arguments that
    /// don't match the tool's input schema.
    ToolError = 1,
    /// The request was rejected before any tool ran, e.g. an unknown tool name.
    InvalidParams = 2,
    /// Any other protocol-level error.
    Internal = 3,
}

impl ToolOutcome {
    pub(crate) fn of(result: &Result<CallToolResponse, ErrorData>) -> Self {
        match result {
            Ok(CallToolResponse::Complete(result)) if result.is_error == Some(true) => {
                Self::ToolError
            }
            Ok(_) => Self::Success,
            Err(error) if error.code == ErrorCode::INVALID_PARAMS => Self::InvalidParams,
            Err(_) => Self::Internal,
        }
    }
}

/// The transport a session is served over. `mirrord mcp` only serves stdio, but the value is
/// reported so events stay comparable if another transport is added.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
enum McpTransport {
    Stdio = 1,
}

/// MCP clients, recognized from the `clientInfo.name` they announce. The names are not
/// standardized, so this matches on fragments of what the clients are known to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum McpClient {
    Other = 0,
    ClaudeCode = 1,
    /// Claude clients other than Claude Code, e.g. the desktop app.
    Claude = 2,
    Cursor = 3,
    VsCode = 4,
    Windsurf = 5,
    Codex = 6,
    GeminiCli = 7,
}

impl McpClient {
    fn from_name(name: &str) -> Self {
        let name = name.to_lowercase();
        // Order matters: Cursor announces itself as `cursor-vscode`.
        if name.contains("claude-code") {
            Self::ClaudeCode
        } else if name.contains("claude") {
            Self::Claude
        } else if name.contains("cursor") {
            Self::Cursor
        } else if name.contains("visual studio code") || name.contains("vscode") {
            Self::VsCode
        } else if name.contains("windsurf") {
            Self::Windsurf
        } else if name.contains("codex") {
            Self::Codex
        } else if name.contains("gemini") {
            Self::GeminiCli
        } else {
            Self::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::CallToolResult;

    use super::*;

    #[test]
    fn client_names() {
        for (name, expected) in [
            ("claude-code", McpClient::ClaudeCode),
            ("claude-ai", McpClient::Claude),
            ("cursor-vscode", McpClient::Cursor),
            ("Visual Studio Code", McpClient::VsCode),
            ("Visual Studio Code - Insiders", McpClient::VsCode),
            ("windsurf-client", McpClient::Windsurf),
            ("codex-mcp-client", McpClient::Codex),
            ("gemini-cli-mcp-client", McpClient::GeminiCli),
            ("some-agent", McpClient::Other),
        ] {
            assert_eq!(McpClient::from_name(name), expected, "{name}");
        }
    }

    #[test]
    fn tool_names() {
        assert_eq!("validate_config".parse(), Ok(McpTool::ValidateConfig));
        assert_eq!(
            "explain_config_option".parse(),
            Ok(McpTool::ExplainConfigOption)
        );
        assert_eq!("get_skill".parse(), Ok(McpTool::GetSkill));
        assert_eq!("search_docs".parse(), Ok(McpTool::SearchDocs));
        assert_eq!("read_doc".parse(), Ok(McpTool::ReadDoc));
        assert!("unknown".parse::<McpTool>().is_err());
        assert!("no_such_tool".parse::<McpTool>().is_err());
    }

    #[test]
    fn tool_outcomes() {
        assert_eq!(
            ToolOutcome::of(&Ok(CallToolResult::success(vec![]).into())),
            ToolOutcome::Success
        );
        assert_eq!(
            ToolOutcome::of(&Ok(CallToolResult::error(vec![]).into())),
            ToolOutcome::ToolError
        );
        assert_eq!(
            ToolOutcome::of(&Err(ErrorData::invalid_params("tool not found", None))),
            ToolOutcome::InvalidParams
        );
        assert_eq!(
            ToolOutcome::of(&Err(ErrorData::internal_error("boom", None))),
            ToolOutcome::Internal
        );
    }
}
