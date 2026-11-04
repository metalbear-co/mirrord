//! Share link state of a session started with `share`.
//!
//! The operator owns the link. It creates the link when the session connects, keeps it while the
//! session's connections live and the entry port (the port that the link opens) is stolen, and
//! serves it to the session's owner in the session's `sessions` view. The CLI reads the link from
//! there before it starts the application and hands it to the intproxy ([`LocalShareLink`]).
//!
//! The intproxy then learns when the link starts working by reading the same view again
//! ([`ShareStatusSource`]), once an agent confirms that it steals the entry port: the operator
//! publishes the link on that same confirmation. A routed agent that the operator lost, or a link
//! it failed to publish, ends the session with `DaemonMessage::Close`, whose reason is the failure.
//!
//! The intproxy only has an application print what the user must know about the link, each thing
//! once: the intproxy runs detached, so only an application's process writes to the user's
//! terminal.

use std::{convert::Infallible, ops::Not, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use mirrord_config::{LayerConfig, feature::network::incoming::IncomingConfig};
use mirrord_intproxy_protocol::{LayerId, ShareReport};
use mirrord_operator::{
    client::{OperatorApi, OperatorSession},
    crd::{SessionShare, SessionSharePhase},
};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    error::ProxyRuntimeError,
    main_tasks::ProxyMessage,
};

/// The environment variable in which the CLI hands [`LocalShareLink`] to the intproxy, as JSON.
pub const LOCAL_SHARE_LINK_ENV: &str = "MIRRORD_LOCAL_SHARE_LINK";

/// How long a failure waits for the application to print it before the intproxy terminates the
/// application anyway. The intproxy doesn't learn whether the application printed it, and the
/// application may never print it, for example when its stderr is a pipe nobody reads.
const FAILURE_REPORT_TIMEOUT: Duration = Duration::from_millis(250);

/// The first wait between two reads of a link that isn't ready yet. The operator publishes the
/// link on the same steal confirmation that starts the reads, so the first reads usually find it
/// ready, and later ones back off.
const FIRST_POLL_DELAY: Duration = Duration::from_millis(100);

/// The longest wait between two reads of a link that isn't ready yet.
const MAX_POLL_DELAY: Duration = Duration::from_secs(2);

/// How long the link may stay not ready before the intproxy logs that it still waits. The reads
/// go on: the operator ends a link that it can't publish, so a link that stays pending is one that
/// the operator still works on.
const SLOW_POLL_LOG_AFTER: Duration = Duration::from_secs(30);

/// The share link that the operator created for this session when it connected, as the CLI read it
/// before it started the application. It doesn't work until the operator publishes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShareLink {
    /// The URL that share-ingress routes to the session.
    pub url: String,
    /// The port of the target that the link opens.
    pub entry_port: u16,
}

/// The error of a [`ShareStatusSource`] read. The intproxy only logs it and reads again.
pub type ShareStatusError = Box<dyn std::error::Error + Send + Sync>;

/// Where the intproxy reads the share link's phase from.
///
/// [`OperatorShareStatus`] reads it from the operator. Tests read it from a fake, so they can drive
/// the intproxy's real printing through every phase without an operator.
pub trait ShareStatusSource: Send + Sync {
    /// Reads the session's share link. `None` means the operator serves no link for the session.
    fn fetch(&self) -> BoxFuture<'static, Result<Option<SessionShare>, ShareStatusError>>;
}

/// Reads the share link from the session's `sessions` view, with the session's certificate.
pub struct OperatorShareStatus {
    config: Arc<LayerConfig>,
    session: Arc<OperatorSession>,
}

impl OperatorShareStatus {
    pub fn new(config: LayerConfig, session: OperatorSession) -> Self {
        Self {
            config: Arc::new(config),
            session: Arc::new(session),
        }
    }
}

impl ShareStatusSource for OperatorShareStatus {
    fn fetch(&self) -> BoxFuture<'static, Result<Option<SessionShare>, ShareStatusError>> {
        let config = self.config.clone();
        let session = self.session.clone();
        Box::pin(async move {
            OperatorApi::session_share(&config, &session)
                .await
                .map_err(Into::into)
        })
    }
}

/// What [`SharePoll`] found out about the link.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum SharePollOutcome {
    /// share-ingress routes the link.
    Ready,
    /// The operator removed the link before it was ready. The message is meant for the user.
    Failed(String),
}

/// Reads the link until the operator publishes it, then reports that to the main task and exits.
///
/// It also exits when the main task drops its sender, for example when the application stops
/// listening on the entry port before the link was ready.
pub(crate) struct SharePoll {
    source: Arc<dyn ShareStatusSource>,
}

impl BackgroundTask for SharePoll {
    type Error = ProxyRuntimeError;
    type MessageIn = Infallible;
    type MessageOut = ProxyMessage;

    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let started = Instant::now();
        let mut delay = FIRST_POLL_DELAY;
        let mut slow_logged = false;

        loop {
            let read = tokio::select! {
                None = message_bus.recv() => return Ok(()),
                read = self.source.fetch() => read,
            };
            let outcome = match read {
                Ok(Some(SessionShare {
                    phase: SessionSharePhase::Ready,
                    ..
                })) => Some(SharePollOutcome::Ready),
                Ok(None
                | Some(SessionShare {
                    phase: SessionSharePhase::Ended,
                    ..
                })) => Some(SharePollOutcome::Failed(
                    "The operator removed the share link before it was ready. Restart the session to get a new link."
                        .to_owned(),
                )),
                // A phase from a newer operator is treated as not ready yet, because only `Ready`
                // tells the intproxy that the link works.
                Ok(Some(SessionShare {
                    phase: SessionSharePhase::Pending | SessionSharePhase::Unknown,
                    ..
                })) => None,
                Err(error) => {
                    tracing::warn!(%error, "Failed to read the share link, reading it again");
                    None
                }
            };
            if let Some(outcome) = outcome {
                message_bus.send(ProxyMessage::ShareStatus(outcome)).await;
                return Ok(());
            }

            if slow_logged.not() && started.elapsed() >= SLOW_POLL_LOG_AFTER {
                tracing::debug!(
                    waited_secs = started.elapsed().as_secs(),
                    "The share link is still not ready"
                );
                slow_logged = true;
            }
            tokio::select! {
                None = message_bus.recv() => return Ok(()),
                _ = tokio::time::sleep(delay) => {}
            }
            delay = (delay * 2).min(MAX_POLL_DELAY);
        }
    }
}

/// A failed link, waiting for an application to print why.
struct Failure {
    message: String,
    /// When the intproxy stops waiting for the application to print the failure.
    deadline: Instant,
    /// Whether an application was asked to print the failure.
    reported: bool,
}

/// How far the operator got with the link.
///
/// `entry_port` is the port that the link opens, in the target, and `local_port` the port that
/// the application listens on for it, after `feature.network.incoming.port_mapping`. The user
/// reads `local_port`.
enum Phase {
    /// The operator registered the link, and share-ingress doesn't route it yet, because no agent
    /// steals the entry port yet. `polling` is set once an agent confirmed the steal and
    /// [`SharePoll`] reads the link.
    Accepted {
        entry_port: u16,
        local_port: u16,
        waiting_reported: bool,
        polling: bool,
    },
    /// share-ingress routes the link. `unprinted_url` is the link until an application prints it.
    Ready {
        entry_port: u16,
        local_port: u16,
        unprinted_url: Option<String>,
    },
    /// The application stopped listening on the entry port, so the operator removed the link for
    /// good. The session keeps running.
    Ended {
        local_port: u16,
        ended_reported: bool,
    },
    Failed(Failure),
}

/// The share link of this session, as the operator reports it, and what an application printed
/// about it.
pub(crate) struct LocalShareState {
    url: String,
    source: Arc<dyn ShareStatusSource>,
    phase: Phase,
}

impl LocalShareState {
    pub(crate) fn new(
        incoming: &IncomingConfig,
        link: LocalShareLink,
        source: Box<dyn ShareStatusSource>,
    ) -> Self {
        let LocalShareLink { url, entry_port } = link;
        let local_port = incoming
            .port_mapping
            .iter()
            .find_map(|(local, remote)| (*remote == entry_port).then_some(*local))
            .unwrap_or(entry_port);
        Self {
            url,
            source: source.into(),
            phase: Phase::Accepted {
                entry_port,
                local_port,
                waiting_reported: false,
                polling: false,
            },
        }
    }

    fn is_failed(&self) -> bool {
        matches!(self.phase, Phase::Failed(_))
    }

    /// Fails the link with `message`, which an application prints. Only the first failure counts.
    pub(crate) fn fail(&mut self, message: String) {
        if self.is_failed() {
            return;
        }
        self.phase = Phase::Failed(Failure {
            message,
            deadline: Instant::now() + FAILURE_REPORT_TIMEOUT,
            reported: false,
        });
    }

    /// Returns the task that reads the link until it is ready, when an agent confirmed that it
    /// steals the entry port. The operator publishes the link on that confirmation, so reading it
    /// earlier would only find it pending.
    ///
    /// Returns it at most once per session, because the link never goes back to waiting.
    pub(crate) fn port_subscribed(&mut self, port: u16) -> Option<SharePoll> {
        match &mut self.phase {
            Phase::Accepted {
                entry_port,
                polling: polling @ false,
                ..
            } if *entry_port == port => {
                *polling = true;
                Some(SharePoll {
                    source: self.source.clone(),
                })
            }
            _ => None,
        }
    }

    /// Applies what [`SharePoll`] found out. It may arrive after the link already ended or failed,
    /// and then changes nothing.
    pub(crate) fn poll_finished(&mut self, outcome: SharePollOutcome) {
        let &Phase::Accepted {
            entry_port,
            local_port,
            ..
        } = &self.phase
        else {
            return;
        };
        match outcome {
            SharePollOutcome::Ready => {
                self.phase = Phase::Ready {
                    entry_port,
                    local_port,
                    unprinted_url: Some(self.url.clone()),
                };
            }
            SharePollOutcome::Failed(message) => self.fail(message),
        }
    }

    /// Ends the link when the intproxy stopped stealing `port`, because no application listens on
    /// it anymore. Returns whether the link ended.
    ///
    /// The operator removes the link once the agent no longer steals the entry port, and doesn't
    /// tell the intproxy, because the session goes on. So the intproxy has the application print
    /// why the link stopped working.
    pub(crate) fn port_released(&mut self, port: u16) -> bool {
        if let Phase::Accepted {
            entry_port,
            local_port,
            ..
        }
        | Phase::Ready {
            entry_port,
            local_port,
            ..
        } = self.phase
            && entry_port == port
        {
            self.phase = Phase::Ended {
                local_port,
                ended_reported: false,
            };
            return true;
        }
        false
    }

    /// Picks the next report an application prints, and the layer that prints it.
    ///
    /// Any process with a control connection can print: the link is ready only after an agent
    /// steals the entry port, so the link works no matter which process prints it. The lowest
    /// [`LayerId`] prints, which is the process that started first among the attached ones.
    pub(crate) fn next_report<'a>(
        &mut self,
        controls: impl Iterator<Item = &'a LayerId>,
    ) -> Option<(LayerId, ShareReport)> {
        let report = match &self.phase {
            Phase::Failed(Failure {
                reported: false,
                message,
                ..
            }) => ShareReport::Failed {
                message: message.clone(),
            },
            Phase::Accepted {
                local_port,
                waiting_reported: false,
                ..
            } => ShareReport::Waiting { port: *local_port },
            Phase::Ready {
                unprinted_url: Some(url),
                ..
            } => ShareReport::Ready { url: url.clone() },
            Phase::Ended {
                local_port,
                ended_reported: false,
            } => ShareReport::Ended { port: *local_port },
            _ => return None,
        };
        let layer = *controls.min_by_key(|layer| layer.0)?;
        match &mut self.phase {
            Phase::Failed(Failure { reported, .. })
            | Phase::Accepted {
                waiting_reported: reported,
                ..
            }
            | Phase::Ended {
                ended_reported: reported,
                ..
            } => *reported = true,
            Phase::Ready { unprinted_url, .. } => *unprinted_url = None,
        }
        Some((layer, report))
    }

    /// When the intproxy stops waiting for the application to print a failure.
    pub(crate) fn failure_deadline(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Failed(failure) => Some(failure.deadline),
            _ => None,
        }
    }

    /// The error that ends the session, once the application had the time to print the failure.
    pub(crate) fn terminal_error(&self) -> Option<ProxyRuntimeError> {
        match &self.phase {
            Phase::Failed(failure) if Instant::now() >= failure.deadline => {
                Some(ProxyRuntimeError::LocalShare(failure.message.clone()))
            }
            _ => None,
        }
    }
}
