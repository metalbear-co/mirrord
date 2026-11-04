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
    client::{OperatorSession, SessionShareReadError, SessionShareReader, error::OperatorApiError},
    crd::{SessionShare, SessionSharePhase},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{sync::OnceCell, time::Instant};

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

/// How long one [`OperatorShareStatus::fetch`] may take, building its client included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long reads of the link may keep failing before the intproxy gives up on the link. Until
/// then a failed read is retried, because the API server or the network may come back.
const READ_FAILURE_DEADLINE: Duration = Duration::from_secs(30);

/// How long the intproxy holds back the report that the operator removed a link that wasn't ready.
/// The operator removes the link first and then ends the session with a `Close` whose reason says
/// why, so a read between the two would otherwise print a vaguer reason than the `Close`.
const REMOVED_LINK_GRACE: Duration = Duration::from_secs(1);

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

/// Why a [`ShareStatusSource`] read failed.
#[derive(Debug, Error)]
pub enum ShareStatusError {
    /// The cluster refused the read, for example because the user's roles or credentials changed
    /// after the session started. Reading again doesn't help, so the link fails with this text,
    /// which is written for the user.
    #[error("{0}")]
    Refused(String),
    /// The read failed in a way that may not last, so the intproxy reads again.
    #[error(transparent)]
    Transient(Box<dyn std::error::Error + Send + Sync>),
}

/// Where the intproxy reads the share link's phase from.
///
/// [`OperatorShareStatus`] reads it from the operator. Tests read it from a fake, so they can drive
/// the intproxy's real printing through every phase without an operator.
pub trait ShareStatusSource: Send + Sync {
    /// Reads the session's share link. `None` means the operator serves no link for the session.
    fn fetch(&self) -> BoxFuture<'static, Result<Option<SessionShare>, ShareStatusError>>;
}

/// Reads the share link from the session's `sessions` view, with the session's certificate.
///
/// The reader is built on the first read and reused after that, so the kubeconfig's credential
/// plugin doesn't run for every read. A failed build is tried again on the next read, except for a
/// session that can't have a link at all. Other build failures, such as a credential plugin that
/// fails, can pass, and the kubeconfig itself already worked for the CLI moments before.
///
/// One timeout covers the build and the read, so a credential plugin that hangs can't hold the
/// poll past [`READ_FAILURE_DEADLINE`].
pub struct OperatorShareStatus {
    config: Arc<LayerConfig>,
    session: Arc<OperatorSession>,
    reader: Arc<OnceCell<SessionShareReader>>,
}

impl OperatorShareStatus {
    pub fn new(config: LayerConfig, session: OperatorSession) -> Self {
        Self {
            config: Arc::new(config),
            session: Arc::new(session),
            reader: Default::default(),
        }
    }
}

impl ShareStatusSource for OperatorShareStatus {
    fn fetch(&self) -> BoxFuture<'static, Result<Option<SessionShare>, ShareStatusError>> {
        let config = self.config.clone();
        let session = self.session.clone();
        let reader = self.reader.clone();
        let fetch = async move {
            let reader = reader
                .get_or_try_init(|| SessionShareReader::new(&config, &session))
                .await
                .map_err(|error| match error {
                    OperatorApiError::LocalShare(message) => ShareStatusError::Refused(message),
                    error => ShareStatusError::Transient(error.into()),
                })?;
            reader.read().await.map_err(|error| match error {
                SessionShareReadError::MissingPermission | SessionShareReadError::Refused(_) => {
                    ShareStatusError::Refused(error.to_string())
                }
                SessionShareReadError::TimedOut | SessionShareReadError::Operator(_) => {
                    ShareStatusError::Transient(error.into())
                }
            })
        };
        Box::pin(async move {
            tokio::time::timeout(FETCH_TIMEOUT, fetch)
                .await
                .unwrap_or_else(|_| {
                    Err(ShareStatusError::Transient(
                        SessionShareReadError::TimedOut.into(),
                    ))
                })
        })
    }
}

/// What [`SharePoll`] found out about the link.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum SharePollOutcome {
    /// share-ingress routes the link.
    Ready,
    /// The operator removed the link before it was ready. A `Close` with the reason may follow;
    /// see [`REMOVED_LINK_GRACE`].
    Removed,
    /// The intproxy can't learn whether the link works. The message is meant for the user.
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
        let mut failing_since = None;

        loop {
            let read_started = Instant::now();
            let read = tokio::select! {
                None = message_bus.recv() => return Ok(()),
                read = self.source.fetch() => read,
            };
            let outcome = match read {
                Ok(Some(SessionShare {
                    phase: SessionSharePhase::Ready,
                    ..
                })) => Some(SharePollOutcome::Ready),
                Ok(
                    None
                    | Some(SessionShare {
                        phase: SessionSharePhase::Ended,
                        ..
                    }),
                ) => Some(SharePollOutcome::Removed),
                // A phase from a newer operator is treated as not ready yet, because only `Ready`
                // tells the intproxy that the link works.
                Ok(Some(SessionShare {
                    phase: SessionSharePhase::Pending | SessionSharePhase::Unknown,
                    ..
                })) => {
                    failing_since = None;
                    None
                }
                Err(ShareStatusError::Refused(message)) => Some(SharePollOutcome::Failed(message)),
                Err(ShareStatusError::Transient(error)) => {
                    let since = *failing_since.get_or_insert(read_started);
                    if since.elapsed() >= READ_FAILURE_DEADLINE {
                        Some(SharePollOutcome::Failed(format!(
                            "The share link couldn't be read from the operator for {} seconds, so mirrord can't tell whether it works ({error}). Restart the session.",
                            READ_FAILURE_DEADLINE.as_secs()
                        )))
                    } else {
                        tracing::warn!(%error, "Failed to read the share link, reading it again");
                        None
                    }
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
    /// When an application may print the failure. It is later than the failure itself only for a
    /// link that the operator removed, whose `Close` may still bring a better reason.
    report_at: Instant,
    /// When the intproxy stops waiting for the application to print the failure.
    deadline: Instant,
    /// Whether an application was asked to print the failure.
    reported: bool,
    /// Whether a later failure replaces this one while it isn't reported yet.
    replaceable: bool,
}

impl Failure {
    fn new(message: String, report_at: Instant, replaceable: bool) -> Self {
        Self {
            message,
            report_at,
            deadline: report_at + FAILURE_REPORT_TIMEOUT,
            reported: false,
            replaceable,
        }
    }
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

    /// Fails the link with `message`, which an application prints.
    ///
    /// The first failure counts, except that a failure replaces the not yet reported report of a
    /// link that the operator removed, because it carries a more specific reason.
    pub(crate) fn fail(&mut self, message: String) {
        if let Phase::Failed(failure) = &self.phase
            && (failure.replaceable.not() || failure.reported)
        {
            return;
        }
        self.phase = Phase::Failed(Failure::new(message, Instant::now(), false));
    }

    /// Whether the reason of a `Close` from the operator is printed as it is, as the share
    /// failure: while the link is registered or ready, or removed with its report still held back.
    /// The operator words the reason of such a `Close` for the user, also when the session ends
    /// for a reason other than the link. After the link ended or its failure was printed, a
    /// `Close` is about the session alone.
    pub(crate) fn prints_close_reason(&self) -> bool {
        match &self.phase {
            Phase::Accepted { .. } | Phase::Ready { .. } => true,
            Phase::Failed(failure) => failure.replaceable && failure.reported.not(),
            Phase::Ended { .. } => false,
        }
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
            SharePollOutcome::Removed => {
                self.phase = Phase::Failed(Failure::new(
                    "The operator removed the share link before it was ready. Restart the session to get a new link."
                        .to_owned(),
                    Instant::now() + REMOVED_LINK_GRACE,
                    true,
                ));
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
                report_at,
                ..
            }) if *report_at <= Instant::now() => ShareReport::Failed {
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

    /// When the intproxy next acts on a failure: it has an application print a failure that was
    /// held back, or it stops waiting for the application to print it.
    pub(crate) fn failure_deadline(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Failed(failure)
                if failure.reported.not() && Instant::now() < failure.report_at =>
            {
                Some(failure.report_at)
            }
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
