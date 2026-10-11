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

/// Upper bound on the number of distinct symbols kept, well above the size of the layer's hook
/// list.
const MAX_SYMBOLS: usize = 512;

/// Upper bound on the length of a symbol name, well above the longest name the layer hooks.
const MAX_SYMBOL_LENGTH: usize = 64;

/// Whether `symbol` looks like the name of a libc function.
///
/// The proxy accepts connections from any local process, so a report is not trusted to hold
/// only names from the layer's hook list. Names end up as keys of the analytics report, where
/// nothing but function names may appear.
fn is_symbol_name(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.len() <= MAX_SYMBOL_LENGTH
        && symbol
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$')
}

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
    pub fn record(&self, report: HooksInstalled) {
        let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);

        let installed = report.installed.into_iter().map(|symbol| (symbol, true));
        let failed = report.failed.into_iter().map(|symbol| (symbol, false));

        for (symbol, installed) in installed.chain(failed) {
            if !is_symbol_name(&symbol)
                || (counts.len() >= MAX_SYMBOLS && !counts.contains_key(&symbol))
            {
                continue;
            }

            let entry = counts.entry(symbol).or_default();
            let count = if installed {
                &mut entry.installed
            } else {
                &mut entry.failed
            };
            *count = count.saturating_add(1);
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
