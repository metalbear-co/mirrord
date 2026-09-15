//! Handler for the `mirrord subscribe` command.
//!
//! Streams the operator's interception events for a session to stdout as JSON, letting test
//! runners assert that interception actually happened. Each payload is forwarded verbatim as
//! opaque JSON; the event schema lives in the operator repo, so this command never deserializes
//! it.

use std::{io::Write, ops::Not};

use futures::{Stream, StreamExt, io::AsyncBufReadExt};
use http::Request;
use kube::Client;
use mirrord_config::{LayerConfig, config::ConfigContext};
use tracing::Level;

use crate::{
    CliResult, config::SubscribeArgs, error::CliError, kube::kube_client_from_layer_config,
    util::remove_proxy_env,
};

/// What an event stream asks the operator to include beyond the default payload. An operator that
/// predates an option ignores it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EventStreamOptions {
    /// Name the session on each event. Implied by a stream that named no key.
    pub session_key_field: bool,

    /// Also deliver queue messages that matched no session's filter.
    pub unmatched: bool,
}

/// Opens the operator's interception event stream for one session, or for every session when
/// `key` is `None`, yielding each event's payload.
///
/// `watch=true` is mandatory: without it the kube-apiserver cuts the connection at its 60s
/// `--request-timeout` instead of treating this as a long-running watch.
pub(crate) async fn operator_event_stream(
    client: &Client,
    key: Option<&str>,
    options: EventStreamOptions,
) -> CliResult<impl Stream<Item = std::io::Result<String>>> {
    let session_key = key
        .map(|key| {
            let encoded: String = url::form_urlencoded::byte_serialize(key.as_bytes()).collect();
            format!("&session_key={encoded}")
        })
        .unwrap_or_default();
    let request = Request::get(format!(
        "/apis/operator.metalbear.co/v1/events?watch=true{session_key}\
         &include_session_key={}&include_unmatched={}",
        options.session_key_field, options.unmatched
    ))
    .body(Vec::new())
    .map_err(|error| CliError::SubscribeError(error.to_string()))?;

    let stream = client
        .request_stream(request)
        .await
        .map_err(|error| CliError::SubscribeError(error.to_string()))?;

    Ok(stream.lines().filter_map(|line| async move {
        match line {
            Ok(line) => {
                let payload = line.strip_prefix("data:")?;
                Some(Ok(payload.strip_prefix(' ').unwrap_or(payload).to_owned()))
            }
            Err(error) => Some(Err(error)),
        }
    }))
}

/// Streams interception events for a session key from the operator to stdout as JSON.
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

    let options = EventStreamOptions {
        session_key_field: args.session_key_field,
        unmatched: args.unmatched,
    };
    let mut events = std::pin::pin!(operator_event_stream(&client, Some(key), options).await?);

    eprintln!("Subscribed to events for session key `{key}`.");

    let mut stdout = std::io::stdout();
    while let Some(payload) = events.next().await {
        let payload = payload.map_err(|error| CliError::SubscribeError(error.to_string()))?;

        let output = if args.pretty {
            let value: serde_json::Value = serde_json::from_str(&payload)?;
            serde_json::to_string_pretty(&value)?
        } else {
            payload
        };

        writeln!(stdout, "{output}")
            .map_err(|error| CliError::SubscribeError(error.to_string()))?;
    }

    Ok(())
}
