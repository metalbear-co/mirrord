//! Handler for the `mirrord subscribe` command.
//!
//! Streams the operator's interception events for a session to stdout as JSON, letting test
//! runners assert that interception actually happened. Each payload is forwarded verbatim as
//! opaque JSON; the event schema lives in the operator repo, so this command never deserializes
//! it.

use std::{io::Write, ops::Not};

use futures::{StreamExt, io::AsyncBufReadExt};
use http::Request;
use mirrord_config::{LayerConfig, config::ConfigContext};
use tracing::Level;

use crate::{
    CliResult, config::SubscribeArgs, error::CliError, kube::kube_client_from_layer_config,
    util::remove_proxy_env,
};

/// Streams interception events for a session key from the operator to stdout as JSON.
///
/// `watch=true` is mandatory: without it the kube-apiserver cuts the connection at its 60s
/// `--request-timeout` instead of treating this as a long-running watch.
#[tracing::instrument(level = Level::TRACE, skip_all, err)]
pub(crate) async fn subscribe_command(args: SubscribeArgs) -> CliResult<()> {
    let mut cfg_context = ConfigContext::default().override_envs(args.as_env_vars());
    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    if layer_config.use_proxy.not() {
        remove_proxy_env();
    }

    let key = layer_config
        .key
        .provided()
        .ok_or(CliError::SessionKeyRequired)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    let encoded_key: String = url::form_urlencoded::byte_serialize(key.as_bytes()).collect();
    let request = Request::get(format!(
        "/apis/operator.metalbear.co/v1/events?watch=true&session_key={encoded_key}"
    ))
    .body(Vec::new())
    .map_err(|error| CliError::SubscribeError(error.to_string()))?;

    let stream = client
        .request_stream(request)
        .await
        .map_err(|error| CliError::SubscribeError(error.to_string()))?;

    eprintln!("Subscribed to events for session key `{key}`.");

    let mut lines = stream.lines();
    let mut stdout = std::io::stdout();
    while let Some(line) = lines.next().await {
        let line = line.map_err(|error| CliError::SubscribeError(error.to_string()))?;

        // Skip `:` keep-alive comments and blank separator lines; keep only `data:` payloads.
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.strip_prefix(' ').unwrap_or(payload);

        let output = if args.pretty {
            let value: serde_json::Value = serde_json::from_str(payload)?;
            serde_json::to_string_pretty(&value)?
        } else {
            payload.to_owned()
        };

        writeln!(stdout, "{output}")
            .map_err(|error| CliError::SubscribeError(error.to_string()))?;
    }

    Ok(())
}
