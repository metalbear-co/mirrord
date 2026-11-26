//! The platform an agent reports about its target, read from its registration metadata.
//!
//! sessions-manager treats an agent's [`AgentMetadata`] as opaque and only copies it into the
//! assignment of every intproxy paired with that agent, so this is where the client side gives it
//! meaning. The metadata is URL-query encoded
//! (`platform=aws-lambda&invocation_port=8080&remote_paths=/mnt/data`).
//!
//! Parsing is lenient on purpose: the agent may be newer than this client and report a platform or
//! keys this client doesn't know, and none of that may keep a session from starting. Anything that
//! can't be understood is dropped, and the session runs as it would without metadata.

use std::path::PathBuf;

use mirrord_sessions_manager_protocol::AgentMetadata;
use serde::{Deserialize, Serialize};

/// A target whose platform changes how the CLI runs the user's process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "kebab-case")]
pub enum TargetPlatform {
    /// An AWS Lambda function, whose invocations a hub agent delivers as HTTP requests.
    AwsLambda(AwsLambdaTarget),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwsLambdaTarget {
    /// The port the hub delivers invocations to, which the local runtime listens on.
    pub invocation_port: u16,
    /// Mount paths (EFS) the hub serves file operations for. Every other path stays local, since
    /// the hub can't reach the function's own filesystem.
    pub remote_paths: Vec<PathBuf>,
}

impl TargetPlatform {
    /// Reads the platform from an agent's metadata, or [`None`] when it reports no platform this
    /// client knows how to use.
    ///
    /// `remote_paths` is comma-separated, the same as the hub's `MIRRORD_HUB_REMOTE_PATHS`.
    pub fn from_metadata(metadata: &AgentMetadata) -> Option<Self> {
        let mut platform = None;
        let mut invocation_port = None;
        let mut remote_paths = Vec::new();

        for (key, value) in url::form_urlencoded::parse(metadata.as_str().as_bytes()) {
            match key.as_ref() {
                "platform" => platform = Some(value.into_owned()),
                "invocation_port" => invocation_port = Some(value.into_owned()),
                "remote_paths" => remote_paths.extend(
                    value
                        .split(',')
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from),
                ),
                _ => {}
            }
        }

        match platform.as_deref()? {
            "aws-lambda" => {
                let Some(invocation_port) = invocation_port
                    .as_deref()
                    .and_then(|port| port.parse::<u16>().ok())
                else {
                    tracing::warn!(
                        "Ignoring the agent's AWS Lambda metadata, it has no valid invocation port"
                    );
                    return None;
                };

                Some(Self::AwsLambda(AwsLambdaTarget {
                    invocation_port,
                    remote_paths,
                }))
            }
            other => {
                tracing::debug!(platform = other, "Ignoring an unknown agent platform");
                None
            }
        }
    }
}
