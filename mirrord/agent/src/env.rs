use std::collections::{HashMap, HashSet};

use mirrord_protocol::RemoteResult;
use tracing::trace;
use wildmatch::WildMatch;

struct EnvFilter {
    include: Vec<WildMatch>,
    exclude: Vec<WildMatch>,
}

impl EnvFilter {
    fn new(select_env_vars: HashSet<String>, filter_env_vars: HashSet<String>) -> Self {
        let include = if select_env_vars.is_empty() {
            vec![WildMatch::new("*")]
        } else {
            select_env_vars
                .iter()
                .map(|selector| WildMatch::new(selector))
                .collect()
        };

        let exclude = {
            let mut exclude = vec![
                WildMatch::new("PATH"),
                WildMatch::new("HOME"),
                WildMatch::new("HOMEPATH"),
                WildMatch::new("CLASSPATH"),
                WildMatch::new("JAVA_EXE"),
                WildMatch::new("JAVA_HOME"),
            ];

            for selector in &filter_env_vars {
                exclude.push(WildMatch::new(selector));
            }

            exclude
        };

        EnvFilter { include, exclude }
    }

    fn matches(&self, key: &str) -> bool {
        !self.exclude.iter().any(|wild| wild.matches(key))
            && self.include.iter().any(|wild| wild.matches(key))
    }
}

/// Helper function that loads the process' environment variables, and selects only those that were
/// requested from `mirrord-layer` (ignores vars specified in `filter_env_vars`).
///
/// NOTE: can remove `RemoteResult` when we break protocol compatibility.
pub(crate) fn select_env_vars(
    full_env: &HashMap<String, String>,
    filter_env_vars: HashSet<String>,
    select_env_vars: HashSet<String>,
) -> RemoteResult<HashMap<String, String>> {
    trace!(
        "select_env_vars -> filter_env_vars {:#?} select_env_vars {:#?}",
        filter_env_vars,
        select_env_vars
    );

    let env_filter = EnvFilter::new(select_env_vars, filter_env_vars);

    let env_vars = full_env
        .iter()
        .filter(|(key, _)| env_filter.matches(key))
        .map(|(a, b)| (a.clone(), b.clone()))
        // [("DB", "foo.db")]
        .collect::<HashMap<_, _>>();

    Ok(env_vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default() {
        let filter = EnvFilter::new(Default::default(), Default::default());

        assert!(filter.matches("FOOBAR"));
    }

    #[test]
    fn include() {
        let filter = EnvFilter::new(
            vec!["FOO", "BAR", "FOOBAR_*"]
                .into_iter()
                .map(|val| val.to_owned())
                .collect(),
            Default::default(),
        );

        assert!(filter.matches("FOO"));
        assert!(!filter.matches("FOOBAR"));

        assert!(filter.matches("BAR"));
        assert!(!filter.matches("BAR_STOOL"));

        assert!(filter.matches("FOOBAR_TEST"));
    }

    #[test]
    fn default_exclude() {
        let filter = EnvFilter::new(
            Default::default(),
            vec!["FOO", "BAR", "FOOBAR_*"]
                .into_iter()
                .map(|val| val.to_owned())
                .collect(),
        );

        assert!(!filter.matches("HOME"));
        assert!(!filter.matches("PATH"));

        assert!(!filter.matches("FOO"));
        assert!(filter.matches("FOOBAR"));

        assert!(!filter.matches("BAR"));
        assert!(filter.matches("BAR_STOOL"));

        assert!(!filter.matches("FOOBAR_TEST"));
    }
}
