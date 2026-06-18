//! Defines types used to report mirrord chaos usage metrics with the [`AnalyticsReporter`] created
//! during `internal_proxy::proxy()` in the CLI. The reporter is held in the session monitor
//! [`AppState`](crate::session_monitor::api::AppState) and accessed during calls made to the
//! [`chaos_router Router`](crate::session_monitor::chaos::api::chaos_router()).

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::Ordering,
    time::Instant,
};

use mirrord_analytics::{AnalyticValue, Analytics, AnalyticsReporter, CollectAnalytics, Reporter};

use crate::session_monitor::chaos::rules::{ChaosRule, ChaosSelector};

/// Wraps AnalyticsReporter for use reporting chaos rule analytics. Stores deleted rules instead of
/// sending anaytics every time a rule is deleted. Sends all rules that were in effect over the
/// whole session when the session ends.
pub(crate) struct ChaosAnalyticsReporter {
    /// used to report analytics on session end
    inner: Option<AnalyticsReporter>,

    /// rules that are currently in use, and the `Instant` they were created
    live_rules: HashMap<ChaosRule, Instant>,

    /// rules that have been stopped, and how long they were live for
    dead_rules: HashSet<ChaosRuleInfo>,
}

impl ChaosAnalyticsReporter {
    pub fn new(inner: Option<AnalyticsReporter>) -> Self {
        Self {
            inner,
            live_rules: HashMap::default(),
            dead_rules: HashSet::default(),
        }
    }

    /// Add a newly created chaos rule to `self.live_rules`. Stores a clone of the rule and its
    /// creation time to allow us to calculate its lifetime on `Drop`.
    pub fn create_rule(&mut self, rule: &ChaosRule) {
        self.live_rules.insert(rule.clone(), Instant::now());
    }

    /// Remove a newly deleted rule that was in effect from `self.live_rules`, and move it to
    /// `self.dead_rules` after converting it into a [`ChaosRuleInfo`]. This is the data that will
    /// be sent in analytics.
    ///
    /// If the rule being deleted wasn't found in `self.live_rules`, log a `debug!` and ignore.
    pub fn delete_rule(&mut self, rule: &ChaosRule) {
        match self.live_rules.remove(&rule.clone()) {
            Some(start_time) => {
                self.dead_rules.insert((rule, start_time).into());
            }
            None => {
                tracing::debug!(
                    ?rule.id,
                    "a chaos rule got deleted but we didn't have it stored in analytics - the rule will not be reported"
                );
            }
        };
    }

    // Remove all live rules from `self.live_rules`, and move them to `self.dead_rules` after
    // converting each into a [`ChaosRuleInfo`].
    pub fn clear_session_rules(&mut self) {
        self.live_rules.drain().for_each(|(rule, start_time)| {
            self.dead_rules.insert((&rule, start_time).into());
        });
    }
}

impl Drop for ChaosAnalyticsReporter {
    fn drop(&mut self) {
        // kill all the live rules we have
        self.live_rules
            .clone()
            .iter()
            .for_each(|(rule, _)| self.delete_rule(rule));

        // report all the dead rules to the AnalyticsReporter
        let rules_info: Vec<_> = self.dead_rules.drain().map(AnalyticValue::from).collect();
        if let Some(x) = self.inner.as_mut() {
            x.get_mut().add("rules", rules_info)
        }
    }
}

/// The type used to report chaos rule analytics with [`AnalyticsReporter`]. Doesn't contain any
/// identifying information, just metadata about field usage etc.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct ChaosRuleInfo {
    /// `ChaosEffectType`'s repr(u8)
    effect_type: u8,
    /// `ChaosSelectorType`'s repr(u8)
    selector_type: u8,
    /// If a custom http filter was set - `true` if selector.http_filter is `Some`
    custom_http_filter_set: bool,
    /// Value of selector.percentage
    selector_percentage: u32,
    /// Final hit count i.e. times a rule was applied
    final_hit_count: u32,
    /// Total rule lifetime in seconds
    rule_lifetime_secs: u32,
}

impl From<(&ChaosRule, Instant)> for ChaosRuleInfo {
    fn from(value: (&ChaosRule, Instant)) -> Self {
        let (rule, start_time) = value;
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
        assert_eq!(expected_rule_info, (&chaos_rule, start_time).into());

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
