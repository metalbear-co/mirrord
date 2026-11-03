//! Share link state of a session started with `share`.
//!
//! The operator owns the link. It keeps the link while this session's connection to it lives and
//! the entry port (the port that the link opens) is stolen, so the intproxy sends it nothing after
//! [`LocalShareClientMessage::Start`](mirrord_protocol::local_share::LocalShareClientMessage::Start).
//! The intproxy only has an application print what the user must know about the link, each thing
//! once: the intproxy runs detached, so only an application's process writes to the user's
//! terminal.

use std::{collections::HashMap, time::Duration};

use mirrord_config::feature::network::incoming::IncomingConfig;
use mirrord_intproxy_protocol::{LayerId, ShareReport};
use mirrord_protocol::local_share::LocalShareEvent;
use tokio::time::Instant;

use crate::error::ProxyRuntimeError;

/// How long a failure waits for the application to print it before the intproxy terminates the
/// application anyway. The intproxy doesn't learn whether the application printed it, and the
/// application may never print it, for example when its stderr is a pipe nobody reads.
const FAILURE_REPORT_TIMEOUT: Duration = Duration::from_millis(250);

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
    /// The intproxy asked for a link, and the operator hasn't registered it yet.
    Requested,
    /// The operator registered the link, and share-ingress doesn't route it yet, because the
    /// agent doesn't steal the entry port yet.
    Accepted {
        entry_port: u16,
        local_port: u16,
        waiting_reported: bool,
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
    /// The port the user listed first in `feature.network.incoming.ports`, so an
    /// [`LocalShareEvent::Accepted`] for another port is a protocol error. `None` lets the
    /// operator pick the port.
    expected_port: Option<u16>,
    /// `feature.network.incoming.port_mapping`, from the target's port to the local one.
    local_ports: HashMap<u16, u16>,
    phase: Phase,
}

impl LocalShareState {
    pub(crate) fn new(incoming: &IncomingConfig) -> Self {
        Self {
            expected_port: incoming.first_configured_port,
            local_ports: incoming
                .port_mapping
                .iter()
                .map(|(local, remote)| (*remote, *local))
                .collect(),
            phase: Phase::Requested,
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

    pub(crate) fn handle_event(&mut self, event: LocalShareEvent) -> Result<(), ProxyRuntimeError> {
        match (&self.phase, event) {
            (Phase::Failed(_), _) => {}
            (_, LocalShareEvent::Failed { message }) => self.fail(message),
            (Phase::Requested, LocalShareEvent::Accepted { entry_port }) => {
                if entry_port == 0
                    || self
                        .expected_port
                        .is_some_and(|expected| expected != entry_port)
                {
                    return Err(ProxyRuntimeError::LocalShare(format!(
                        "The operator registered the share link on port {entry_port}, which isn't the port this session shares."
                    )));
                }
                self.phase = Phase::Accepted {
                    entry_port,
                    local_port: self
                        .local_ports
                        .get(&entry_port)
                        .copied()
                        .unwrap_or(entry_port),
                    waiting_reported: false,
                };
            }
            (
                &Phase::Accepted {
                    entry_port,
                    local_port,
                    ..
                },
                LocalShareEvent::Ready { url },
            ) => {
                self.phase = Phase::Ready {
                    entry_port,
                    local_port,
                    unprinted_url: Some(url),
                };
            }
            // The operator published the link before it saw the entry port's release, and
            // removed it after.
            (Phase::Ended { .. }, LocalShareEvent::Ready { .. }) => {}
            (_, event) => {
                let event = match event {
                    LocalShareEvent::Accepted { .. } => "Accepted",
                    LocalShareEvent::Ready { .. } => "Ready",
                    LocalShareEvent::Failed { .. } => "Failed",
                };
                return Err(ProxyRuntimeError::LocalShare(format!(
                    "The operator sent {event} for the share link at the wrong time."
                )));
            }
        }
        Ok(())
    }

    /// Ends the link when the intproxy stopped stealing `port`, because no application listens on
    /// it anymore.
    ///
    /// The operator removes the link once the agent no longer steals the entry port, and doesn't
    /// tell the intproxy, because the session goes on. So the intproxy has the application print
    /// why the link stopped working.
    pub(crate) fn port_released(&mut self, port: u16) {
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
        }
    }

    /// Picks the next report an application prints, and the layer that prints it.
    ///
    /// Any process with a control connection can print: the operator sends `Ready` only after the
    /// agent steals the entry port, so the link works no matter which process prints it. The
    /// lowest [`LayerId`] prints, which is the process that started first among the attached ones.
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
            Phase::Requested => {}
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
