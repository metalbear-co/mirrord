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
        if let Some(x) = self.inner.as_mut() {
            x.get_mut().add("rules", rules_info)
        }
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

#[cfg(test)]
mod test {
    use std::{
        sync::{Arc, atomic::AtomicU32},
        time::{Duration, Instant},
    };

    use assert_json_diff::assert_json_eq;
    use mirrord_analytics::{AnalyticValue, Analytics};
    use mirrord_config::feature::network::filter::AddressFilter;
    use rstest::rstest;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use crate::session_monitor::chaos::{
        analytics::ChaosRuleInfo,
        rules::{
            ChaosEffectConnectionError, ChaosEffectLatency, ChaosEffectType, ChaosRule,
            ChaosSelector, ChaosSelectorType, ConnectionErrorType, Percentage, TcpChaosEffect,
        },
    };

    #[rstest]
    #[case::tcp_latency(ChaosRule {
        id: Uuid::default(),
        name: Some("rust-connect-slow".to_owned()),
        selector: ChaosSelector::Tcp {
            upstream: AddressFilter::Name("rust-lang.org".to_owned(), 0),
            percentage: Percentage::from(100),
            effect: TcpChaosEffect::Latency(ChaosEffectLatency::new(
                Duration::from_millis(200),
                Duration::from_millis(50),)
            ),
        },
        priority: 0,
        hit_count: Arc::new(AtomicU32::from(33)),
    }, ChaosRuleInfo {
        effect_type: ChaosEffectType::Latency as u8,
        selector_type: ChaosSelectorType::Tcp as u8,
        custom_http_filter_set: false,
        selector_percentage: 100,
        final_hit_count: 33,
        rule_lifetime_secs: 14
    },
    json!({
        "rule": {
            "effect_type": ChaosEffectType::Latency as u8,
            "selector_type": ChaosSelectorType::Tcp as u8,
            "custom_http_filter_set": false,
            "selector_percentage": 100,
            "final_hit_count": 33,
            "rule_lifetime_secs": 14
        }
    }))]
    #[case::tcp_conn_error(ChaosRule {
        id: Uuid::default(),
        name: None,
        priority: 100,
        selector: ChaosSelector::Tcp {
            upstream: AddressFilter::Name(
                "https://www.gov.pl/web/baza-wiedzy/phishing-jako-najczesciej-spotykana-forma-cyberatakow".to_owned(),
                3030
            ),
            percentage: Percentage::from(75),
            effect: TcpChaosEffect::ConnectionError(ChaosEffectConnectionError {
                error_type: ConnectionErrorType::TimedOut,
                after: Duration::from_millis(750)
            }),
        },
        hit_count: Arc::new(AtomicU32::from(0)),
    }, ChaosRuleInfo {
        effect_type: ChaosEffectType::ConnectionError as u8,
        selector_type: ChaosSelectorType::Tcp as u8,
        custom_http_filter_set: false,
        selector_percentage: 75,
        final_hit_count: 0,
        rule_lifetime_secs: 14
    },
    json!({
        "rule": {
            "effect_type": ChaosEffectType::ConnectionError as u8,
            "selector_type": ChaosSelectorType::Tcp as u8,
            "custom_http_filter_set": false,
            "selector_percentage": 75,
            "final_hit_count": 0,
            "rule_lifetime_secs": 14
        }
    }))]
    fn get_analytics_on_rule(
        #[case] chaos_rule: ChaosRule,
        #[case] expected_rule_info: ChaosRuleInfo,
        #[case] expected_json: Value,
    ) {
        let start_time = Instant::now().checked_sub(Duration::from_secs(14)).unwrap();
        assert_eq!(
            expected_rule_info,
            ChaosRuleInfo::from_rule(&chaos_rule, start_time)
        );

        let mut analytics = Analytics::default();
        analytics.add("rule", expected_rule_info);

        assert_json_eq!(analytics, expected_json)
    }

    #[rstest]
    #[case(vec![
        ChaosRuleInfo {
            effect_type: ChaosEffectType::Latency as u8,
            selector_type: ChaosSelectorType::Tcp as u8,
            custom_http_filter_set: false,
            selector_percentage: 100,
            final_hit_count: 33,
            rule_lifetime_secs: 14
        },
        ChaosRuleInfo {
            effect_type: ChaosEffectType::ConnectionError as u8,
            selector_type: ChaosSelectorType::Tcp as u8,
            custom_http_filter_set: false,
            selector_percentage: 75,
            final_hit_count: 0,
            rule_lifetime_secs: 14
        }
    ],
    json!({
        "rules": [
            {
                "effect_type": ChaosEffectType::Latency as u8,
                "selector_type": ChaosSelectorType::Tcp as u8,
                "custom_http_filter_set": false,
                "selector_percentage": 100,
                "final_hit_count": 33,
                "rule_lifetime_secs": 14
            },
            {
                "effect_type": ChaosEffectType::ConnectionError as u8,
                "selector_type": ChaosSelectorType::Tcp as u8,
                "custom_http_filter_set": false,
                "selector_percentage": 75,
                "final_hit_count": 0,
                "rule_lifetime_secs": 14
            }
        ]
    }))]
    fn get_analytics_on_vec_rules(
        #[case] chaos_rules_info: Vec<ChaosRuleInfo>,
        #[case] expected_json: Value,
    ) {
        let mut analytics = Analytics::default();
        let chaos_rules: Vec<_> = chaos_rules_info
            .into_iter()
            .map(AnalyticValue::from)
            .collect();
        analytics.add("rules", chaos_rules);

        assert_json_eq!(analytics, expected_json)
    }
}
