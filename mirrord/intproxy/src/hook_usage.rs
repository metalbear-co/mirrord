//! Session-wide record of which libc hooks the layers managed to install.
//!
//! The layer hooks a fixed list of symbols, and part of that list does not exist on every
//! platform. Knowing, per platform and symbol, whether the hook was installed in any process
//! and whether it failed in any shows which hooks never install on a given platform and can be
//! dropped there.

use std::collections::HashMap;

use mirrord_analytics::{AnalyticValue, Analytics, CollectAnalytics};
use mirrord_intproxy_protocol::HooksInstalled;

/// Upper bound on the number of distinct symbols kept, well above the size of the layer's hook
/// list.
const MAX_SYMBOLS: usize = 512;

/// Upper bound on the number of distinct platforms kept. The layers of a session rarely run on
/// more than one.
const MAX_PLATFORMS: usize = 8;

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

/// Whether any layer installed the hook of a symbol, and whether any failed to. Both are set
/// when the outcome differs between processes.
#[derive(Debug, Default, Clone, Copy)]
struct HookOutcome {
    installed: bool,
    failed: bool,
}

/// CPU architecture and operating system version of a layer, as it reports them.
type Platform = (u32, Option<(u32, u32)>);

/// Hook outcomes of the layers of a session, one record per platform the layers ran on.
///
/// Every layer reports, and layers on one platform mostly report the same, so their reports
/// are merged instead of counted.
#[derive(Debug, Default)]
pub struct HookUsage(HashMap<Platform, HashMap<String, HookOutcome>>);

impl HookUsage {
    pub fn record(&mut self, report: HooksInstalled) {
        let platform = (report.arch, report.os_version);
        if self.0.len() >= MAX_PLATFORMS && !self.0.contains_key(&platform) {
            return;
        }
        let outcomes = self.0.entry(platform).or_default();

        let installed = report.installed.into_iter().map(|symbol| (symbol, true));
        let failed = report.failed.into_iter().map(|symbol| (symbol, false));

        for (symbol, installed) in installed.chain(failed) {
            if !is_symbol_name(&symbol)
                || (outcomes.len() >= MAX_SYMBOLS && !outcomes.contains_key(&symbol))
            {
                continue;
            }

            let outcome = outcomes.entry(symbol).or_default();
            if installed {
                outcome.installed = true;
            } else {
                outcome.failed = true;
            }
        }
    }
}

impl CollectAnalytics for &HookUsage {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        if self.0.is_empty() {
            return;
        }

        let records = self.0.iter().map(|((arch, os_version), outcomes)| {
            let mut symbols = Analytics::default();
            for (symbol, outcome) in outcomes {
                let mut hook = Analytics::default();
                hook.add("installed", outcome.installed);
                hook.add("failed", outcome.failed);
                symbols.add(symbol, hook);
            }

            let mut record = Analytics::default();
            record.add("arch", *arch);
            if let Some((major, minor)) = os_version {
                record.add("os_version_major", *major);
                record.add("os_version_minor", *minor);
            }
            record.add("symbols", symbols);
            AnalyticValue::from(record)
        });

        analytics.add("hooks", records.collect::<Vec<_>>());
    }
}
