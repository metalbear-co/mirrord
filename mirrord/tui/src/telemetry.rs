//! Anonymous usage reporting for the interface.
//!
//! Every action is reported as it happens rather than summarized when the interface exits. A TUI
//! stays open for a long time and is usually closed by closing its terminal window, which is a
//! `SIGHUP` that kills the process before any end-of-run report could be sent - a summary would
//! mostly never arrive, and would be missing precisely the longest sessions. Reporting as things
//! happen costs one request per action and loses only the last one.
//!
//! Every report carries the same `run_id`, so one interface run can be reassembled from its events
//! (and its length read from their timestamps) even when the closing report never arrives.
//!
//! Reporting is off entirely unless the caller supplies a [`Session`]: the standalone `mirrord-tui`
//! binary is a development tool and reports nothing, and `mirrord tui` passes one only when the
//! mirrord config leaves `telemetry` on.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use mirrord_analytics::{Analytics, AnalyticsReporter, Reporter};
use uuid::Uuid;

/// Sent as a number rather than a name because [`mirrord_analytics::AnalyticValue`] deliberately
/// has no string variant, which is what keeps target names and namespaces out of telemetry by
/// construction. `execution_kind` is reported the same way.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
enum Action {
    Started = 1,
    ConnectionFailed = 2,
    SessionStarted = 3,
    PreviewEnvsStopped = 4,
    Closed = 5,
}

pub struct Session {
    pub enabled: bool,
    /// Random per-machine identifier, from the CLI's user data.
    pub machine_id: Uuid,
    /// Keeps the runtime alive until an in-flight report finishes.
    pub watch: drain::Watch,
}

/// Cheap to clone: every screen gets one through [`crate::context::Context`].
#[derive(Clone, Default)]
pub struct Telemetry(Option<Arc<Inner>>);

struct Inner {
    machine_id: Uuid,
    watch: drain::Watch,
    /// Ties this run's reports together.
    run_id: Uuid,
    /// How many times each tab was switched to, reported when the interface closes. Ordered so
    /// the reported object is stable rather than however the map happened to hash.
    tab_visits: Mutex<BTreeMap<&'static str, u32>>,
}

impl Telemetry {
    pub fn disabled() -> Self {
        Self(None)
    }

    /// Reports this run under a fresh `run_id`, unless `session` is disabled.
    pub fn new(session: Option<Session>) -> Self {
        let Some(session) = session.filter(|session| session.enabled) else {
            return Self::disabled();
        };

        Self(Some(Arc::new(Inner {
            machine_id: session.machine_id,
            watch: session.watch,
            run_id: Uuid::new_v4(),
            tab_visits: Default::default(),
        })))
    }

    pub fn started(&self) {
        self.report(Action::Started, |_| {});
    }

    /// Reported without the reason: the error text is arbitrary, and the point is how often
    /// people are blocked here at all.
    pub fn connection_failed(&self) {
        self.report(Action::ConnectionFailed, |_| {});
    }

    /// A mirrord session was launched from the targets view.
    pub fn session_started(&self) {
        self.report(Action::SessionStarted, |_| {});
    }

    /// Preview environments were stopped, `count` of them by the one command.
    pub fn preview_envs_stopped(&self, count: u32) {
        self.report(Action::PreviewEnvsStopped, |analytics| {
            analytics.add("preview_envs_stopped", count);
        });
    }

    /// The user switched to `tab`. Counted rather than reported, and sent by [`Self::closed`].
    pub fn tab_visited(&self, tab: &'static str) {
        let Some(inner) = &self.0 else { return };

        if let Ok(mut visits) = inner.tab_visits.lock() {
            *visits.entry(tab).or_default() += 1;
        }
    }

    /// The interface is exiting gracefully. Report how often each tab was visited.
    ///
    /// This event will be lost whenever the TUI is killed rather than quit with `q` or `Ctrl+C`.
    pub fn closed(&self) {
        let Some(inner) = &self.0 else { return };

        let visits = match inner.tab_visits.lock() {
            Ok(visits) => visits.clone(),
            Err(_) => return,
        };

        self.report(Action::Closed, |analytics| {
            let mut tabs = Analytics::default();
            for (tab, count) in visits {
                tabs.add(tab, count);
            }
            analytics.add("tab_visits", tabs);
        });
    }

    /// Builds one report and hands it to the analytics pipeline, which sends it when the reporter
    /// is dropped at the end of this function.
    fn report(&self, action: Action, extra: impl FnOnce(&mut Analytics)) {
        let Some(inner) = &self.0 else { return };

        let mut reporter = AnalyticsReporter::for_tui_event(
            true,
            inner.watch.clone(),
            inner.machine_id,
            inner.run_id,
        );

        let analytics = reporter.get_mut();
        analytics.add("tui_action", action as u32);
        extra(analytics);
    }
}
