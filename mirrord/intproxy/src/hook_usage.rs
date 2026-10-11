//! Session-wide record of which libc hooks the layers managed to install.
//!
//! The layer hooks a fixed list of symbols, and part of that list does not exist on every
//! platform. Counting, per symbol, how many layers installed the hook and how many failed to
//! shows which hooks never install on a given platform and can be dropped there.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_intproxy_protocol::HooksInstalled;

#[derive(Debug, Default, Clone, Copy)]
struct HookCounts {
    installed: u32,
    failed: u32,
}

/// Shared handle to the counts, so that they can be read after the proxy that fills them is
/// consumed by its run loop.
#[derive(Debug, Default, Clone)]
pub struct HookUsage(Arc<Mutex<HashMap<String, HookCounts>>>);

impl HookUsage {
    pub(crate) fn record(&self, report: HooksInstalled) {
        let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);

        for symbol in report.installed {
            let entry = counts.entry(symbol).or_default();
            entry.installed = entry.installed.saturating_add(1);
        }
        for symbol in report.failed {
            let entry = counts.entry(symbol).or_default();
            entry.failed = entry.failed.saturating_add(1);
        }
    }
}

impl CollectAnalytics for &HookUsage {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        let counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);

        for (symbol, counts) in counts.iter() {
            let mut hook = Analytics::default();
            hook.add("installed", counts.installed);
            hook.add("failed", counts.failed);
            analytics.add(symbol, hook);
        }
    }
}

#[cfg(test)]
mod tests {
    use mirrord_analytics::{Analytics, CollectAnalytics};
    use mirrord_intproxy_protocol::HooksInstalled;

    use super::HookUsage;

    #[test]
    fn counts_layers_per_symbol() {
        let usage = HookUsage::default();
        usage.record(HooksInstalled {
            installed: vec!["open".to_owned(), "connect".to_owned()],
            failed: vec!["open64".to_owned()],
        });
        usage.record(HooksInstalled {
            installed: vec!["open".to_owned()],
            failed: vec!["open64".to_owned(), "connect".to_owned()],
        });

        let mut analytics = Analytics::default();
        (&usage).collect_analytics(&mut analytics);

        assert_eq!(
            serde_json::to_value(&analytics).unwrap(),
            serde_json::json!({
                "open": { "installed": 2, "failed": 0 },
                "connect": { "installed": 1, "failed": 1 },
                "open64": { "installed": 0, "failed": 2 },
            }),
        );
    }
}
