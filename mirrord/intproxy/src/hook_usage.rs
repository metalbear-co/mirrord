//! Session-wide record of which libc hooks the layers managed to install.
//!
//! The layer hooks a fixed list of symbols, and part of that list does not exist on every
//! platform. Counting, per symbol, how many layers installed the hook and how many failed to
//! shows which hooks never install on a given platform and can be dropped there.

use std::collections::HashMap;

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

/// Per symbol, how many layers installed its hook and how many failed to.
#[derive(Debug, Default)]
pub struct HookUsage {
    counts: HashMap<String, HookCounts>,
    /// Architecture and operating system version of the layer that reported last. The layers of
    /// one session normally share them.
    platform: Option<(u32, Option<(u32, u32)>)>,
}

impl HookUsage {
    pub fn record(&mut self, report: HooksInstalled) {
        self.platform = Some((report.arch, report.os_version));
        let counts = &mut self.counts;

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
        let Some((arch, os_version)) = self.platform else {
            return;
        };

        analytics.add("layer_arch", arch);
        if let Some((major, minor)) = os_version {
            analytics.add("layer_os_version_major", major);
            analytics.add("layer_os_version_minor", minor);
        }

        let mut hooks = Analytics::default();
        for (symbol, counts) in &self.counts {
            let mut hook = Analytics::default();
            hook.add("installed", counts.installed);
            hook.add("failed", counts.failed);
            hooks.add(symbol, hook);
        }
        analytics.add("hooks", hooks);
    }
}
