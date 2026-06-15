use std::{
    collections::{HashMap, HashSet},
    sync::atomic::Ordering,
    time::Instant,
};

use mirrord_analytics::{AnalyticValue, Analytics, AnalyticsReporter, CollectAnalytics, Reporter};

use crate::session_monitor::chaos::rules::{ChaosRule, ChaosSelector};

pub(crate) struct ChaosAnalyticsReporter {
    /// used to report analytics on session end
    inner: Option<AnalyticsReporter>,

    /// rules that are currently in use, and the `Instant` they were created
    live_rules: HashMap<ChaosRule, Instant>,

    /// rules that have been stopped, and how long they were live for
    dead_rules: HashSet<ChaosRuleInfo>,
}

/// Wraps AnalyticsReporter for use reporting chaos rule analytics. Stores deleted rules instead of
/// sending anaytics every time a rule is deleted. Sends all rules that were in effect over the
/// whole session when the session ends.
impl ChaosAnalyticsReporter {
    pub fn new(inner: Option<AnalyticsReporter>) -> Self {
        Self {
            inner,
            live_rules: HashMap::default(),
            dead_rules: HashSet::default(),
        }
    }

    // Add a rule when it is created. Stores the rule with its creation time to allow us to
    // calculate its lifetime on Drop.
    pub fn add_live_rule(&mut self, rule: &ChaosRule) {
        self.live_rules.insert(rule.clone(), Instant::now());
    }

    // Remove a rule that was in effect from `self.live_rules`, and move it to `self.dead_rules`
    // after converting it into a [`ChaosRuleInfo`]. This is what will be sent in analytics.
    pub fn kill_live_rule(&mut self, rule: &ChaosRule) {
        match self.live_rules.remove(&rule.clone()) {
            Some(start_time) => {
                let _ = self
                    .dead_rules
                    .insert(ChaosRuleInfo::from_rule(rule, start_time));
            }
            None => {
                tracing::debug!(
                    "BUG: a rule got removed but we didn't have it stored in analytics. The rule will not be reported"
                );
            }
        };
    }

    // Remove all live rules from `self.live_rules`, and move them to `self.dead_rules` after
    // converting each into a [`ChaosRuleInfo`].
    pub fn kill_all_live_rules(&mut self) {
        self.live_rules.drain().for_each(|(rule, start_time)| {
            self.dead_rules
                .insert(ChaosRuleInfo::from_rule(&rule, start_time));
        });
    }
}

impl Drop for ChaosAnalyticsReporter {
    fn drop(&mut self) {
        // kill all the live rules we have
        self.live_rules
            .clone()
            .iter()
            .for_each(|(rule, _)| self.kill_live_rule(rule));

        // report all the dead rules
        let rules_info: Vec<_> = self.dead_rules.drain().map(AnalyticValue::from).collect();
        self.inner
            .as_mut()
            .map(|x| x.get_mut().add("rules", rules_info));
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct ChaosRuleInfo {
    effect_type: u8,              // ChaosEffectType
    selector_type: u8,            // ChaosSelectorType
    custom_http_filter_set: bool, // selector.http_filter != *
    selector_percentage: u32,     // selector.percentage != 100
    final_hit_count: u32,         // hit count
    rule_lifetime_secs: u32,      // lifetime in seconds
}

impl ChaosRuleInfo {
    pub fn from_rule(rule: &ChaosRule, start_time: Instant) -> Self {
        let rule_lifetime_secs = start_time
            .elapsed()
            .as_secs()
            .try_into()
            .unwrap_or(u32::MAX);

        let selector_percentage = rule.selector_percentage().as_percentage();
        let effect_type = rule.effect_type().map(|t| t as u8).unwrap_or(u8::MAX);
        let selector_type = rule.selector_type() as u8;
        let custom_http_filter_set = match &rule.selector {
            ChaosSelector::Http { filter, .. } => filter.is_some(),
            ChaosSelector::Tcp { .. } | ChaosSelector::Fs { .. } | ChaosSelector::None => false,
        };

        ChaosRuleInfo {
            effect_type,
            selector_type,
            custom_http_filter_set,
            selector_percentage,
            final_hit_count: rule.hit_count.load(Ordering::Relaxed),
            rule_lifetime_secs,
        }
    }
}

impl CollectAnalytics for ChaosRuleInfo {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        let &Self {
            effect_type,
            selector_type,
            custom_http_filter_set,
            selector_percentage,
            final_hit_count,
            rule_lifetime_secs,
        } = self;

        analytics.add("effect_type", effect_type as u32);
        analytics.add("selector_type", selector_type as u32);
        analytics.add("custom_http_filter_set", custom_http_filter_set);
        analytics.add("selector_percentage", selector_percentage);
        analytics.add("final_hit_count", final_hit_count);
        analytics.add("rule_lifetime_secs", rule_lifetime_secs);
    }
}
