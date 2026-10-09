//! Handler for the `mirrord subscribe` command.
//!
//! Streams the operator's interception events for a session to stdout as JSON, letting test
//! runners assert that interception actually happened. Each payload is forwarded verbatim as
//! opaque JSON; the event schema lives in the operator repo, so this command never deserializes
//! it.

use std::{io::Write, ops::Not};

use clap::Args;
use futures::{Stream, StreamExt, io::AsyncBufReadExt};
use http::Request;
use kube::{Api, Client};
use mirrord_config::config::ConfigContext;
use mirrord_operator::crd::{MirrordOperatorCrd, NewOperatorFeature, OPERATOR_STATUS_NAME};
use tracing::Level;

use crate::{
    CliResult, config::SubscribeArgs, error::CliError, kube::kube_client_from_layer_config,
    util::remove_proxy_env,
};

/// What an event stream asks the operator to include beyond the default payload. An operator that
/// predates an option ignores it.
///
/// A stream that names no key names the session on every event whatever [`Self::session_key_field`]
/// says, since nothing else tells its events apart.
#[derive(Args, Clone, Copy, Debug, Default)]
pub(crate) struct EventStreamOptions {
    /// Name the session each event belongs to, as a `session_key` field.
    #[arg(long)]
    pub session_key_field: bool,

    /// Also report queue messages that matched no session's filter, as `"mode": "filtered"`.
    ///
    /// These cover every session splitting the queue, including messages that went to a teammate's
    /// session or straight to the workload. HTTP requests that matched nothing are never reported:
    /// the agent passes those to the workload before the operator sees them.
    #[arg(long)]
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

/// What the stream will carry, decided from the operator's advertised features before
/// subscribing, so the command says so up front instead of leaving the user to guess from the
/// events that do or do not show up.
#[derive(Debug, PartialEq, Eq)]
enum StreamScope {
    /// Events intercepted by this operator only. The `note`, when set, says why the stream is
    /// narrower than it could be.
    OneCluster { note: Option<&'static str> },
    /// A multi-cluster primary that relays every linked cluster's events too.
    AllClusters,
}

/// Decides what the stream covers and whether streaming every session is allowed, from the
/// operator's features. `None` features means the operator could not be read.
///
/// Streaming every session needs an operator that serves a keyless stream; with a key any
/// operator works, so a failed operator read does not block a keyed subscription.
fn stream_scope(
    key: Option<&str>,
    features: Option<&[NewOperatorFeature]>,
) -> Result<StreamScope, CliError> {
    let has = |feature| features.is_some_and(|features| features.contains(&feature));

    if key.is_none() && !has(NewOperatorFeature::SubscribeEventOptions) {
        let reason = match features {
            Some(_) => "it serves one session's events at a time",
            None => "its advertised features could not be read",
        };
        return Err(CliError::SubscribeAllSessionsUnsupported(reason.to_owned()));
    }

    if !has(NewOperatorFeature::MultiClusterPrimary) {
        return Ok(StreamScope::OneCluster { note: None });
    }

    if has(NewOperatorFeature::MultiClusterSubscribe) {
        Ok(StreamScope::AllClusters)
    } else {
        Ok(StreamScope::OneCluster {
            note: Some(
                "This multi-cluster operator does not relay events from its linked clusters, so \
                 only events intercepted on this cluster are streamed. Upgrade the operator to \
                 see every cluster.",
            ),
        })
    }
}

/// The features the operator advertises, or the reason they could not be read.
async fn operator_features(client: &Client) -> Result<Vec<NewOperatorFeature>, kube::Error> {
    let operator: MirrordOperatorCrd = Api::all(client.clone()).get(OPERATOR_STATUS_NAME).await?;
    Ok(operator.spec.supported_features())
}

/// Streams interception events for a session key, or for every session, from the operator to
/// stdout as JSON.
#[tracing::instrument(level = Level::TRACE, skip_all, err)]
pub(crate) async fn subscribe_command(args: SubscribeArgs) -> CliResult<()> {
    let mut cfg_context = ConfigContext::default().override_envs(args.as_env_vars());
    let layer_config = crate::util::resolve_layer_config(&mut cfg_context).await?;

    if layer_config.use_proxy.not() {
        remove_proxy_env();
    }

    let key = layer_config.key.provided();
    let client = kube_client_from_layer_config(&layer_config).await?;

    let features = match operator_features(&client).await {
        Ok(features) => Some(features),
        Err(error) => {
            tracing::debug!(%error, "could not read the operator's features before subscribing");
            None
        }
    };
    let scope = stream_scope(key, features.as_deref())?;

    let mut events =
        std::pin::pin!(operator_event_stream(&client, key, args.event_stream_options).await?);

    match key {
        Some(key) => eprintln!("Subscribed to events for session key `{key}`."),
        None => eprintln!("Subscribed to events for every session."),
    }
    match scope {
        StreamScope::AllClusters => {
            eprintln!(
                "Streaming from the primary and its linked clusters; `cluster` names each event's source."
            )
        }
        StreamScope::OneCluster { note: Some(note) } => eprintln!("Warning: {note}"),
        StreamScope::OneCluster { note: None } => {}
    }

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

#[cfg(test)]
mod test {
    use super::*;

    /// A primary from before this feature: it fans sessions out but not events.
    const OLD_PRIMARY: &[NewOperatorFeature] = &[
        NewOperatorFeature::SubscribeEventOptions,
        NewOperatorFeature::MultiClusterPrimary,
    ];

    const PRIMARY: &[NewOperatorFeature] = &[
        NewOperatorFeature::SubscribeEventOptions,
        NewOperatorFeature::MultiClusterPrimary,
        NewOperatorFeature::MultiClusterSubscribe,
    ];

    /// A keyed subscription works against any operator, even one whose features could not be
    /// read, because every operator serves one session's stream.
    #[test]
    fn a_key_subscribes_whatever_the_operator_advertises() {
        assert_eq!(
            stream_scope(Some("k"), None).unwrap(),
            StreamScope::OneCluster { note: None }
        );
        assert_eq!(
            stream_scope(Some("k"), Some(&[])).unwrap(),
            StreamScope::OneCluster { note: None }
        );
    }

    /// Without a key the operator must serve a keyless stream, so an older or unreadable
    /// operator is refused up front instead of silently streaming nothing.
    #[test]
    fn no_key_needs_an_operator_that_streams_every_session() {
        assert!(matches!(
            stream_scope(None, Some(&[])),
            Err(CliError::SubscribeAllSessionsUnsupported(_))
        ));
        assert!(matches!(
            stream_scope(None, None),
            Err(CliError::SubscribeAllSessionsUnsupported(_))
        ));
        assert_eq!(
            stream_scope(None, Some(&[NewOperatorFeature::SubscribeEventOptions])).unwrap(),
            StreamScope::OneCluster { note: None }
        );
    }

    /// A primary that relays its linked clusters streams the whole setup; one that does not is
    /// still subscribed, with a warning that only its own cluster is covered.
    #[test]
    fn a_primary_streams_every_cluster_only_when_it_relays_them() {
        assert_eq!(
            stream_scope(Some("k"), Some(PRIMARY)).unwrap(),
            StreamScope::AllClusters
        );
        assert!(matches!(
            stream_scope(Some("k"), Some(OLD_PRIMARY)).unwrap(),
            StreamScope::OneCluster { note: Some(_) }
        ));
    }
}
