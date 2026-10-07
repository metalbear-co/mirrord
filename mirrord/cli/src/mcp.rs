//! The `mirrord mcp` command - the MCP server for AI agents, implemented in the `mirrord-mcp`
//! crate.
//!
//! The process's stdout carries the MCP protocol, so nothing on this path may print to it; the
//! CLI's logs already go to stderr.

use std::{env, ops::Not};

use mirrord_mcp::McpTelemetry;

use crate::{CliResult, data::UserData};

/// The `mirrord mcp` command handler. Returns when the client disconnects.
pub(crate) async fn mcp_command(watch: drain::Watch, user_data: &UserData) -> CliResult<()> {
    let enabled = env::var("MIRRORD_TELEMETRY")
        .is_ok_and(|value| value == "false")
        .not();

    mirrord_mcp::serve_stdio(McpTelemetry {
        enabled,
        machine_id: user_data.machine_id(),
        watch,
    })
    .await?;

    Ok(())
}
