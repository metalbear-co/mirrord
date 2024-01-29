use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use mirrord_protocol::RemoteResult;
use tokio::io::AsyncReadExt;
use wildmatch::WildMatch;

use crate::error::Result;

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
                WildMatch::new("BUNDLER_ORIG_BUNDLER_ORIG_MANPATH"),
                WildMatch::new("BUNDLER_ORIG_BUNDLER_VERSION"),
                WildMatch::new("BUNDLER_ORIG_BUNDLE_BIN_PATH"),
                WildMatch::new("BUNDLER_ORIG_BUNDLE_GEMFILE"),
                WildMatch::new("BUNDLER_ORIG_GEM_HOME"),
                WildMatch::new("BUNDLER_ORIG_MANPATH"),
                WildMatch::new("BUNDLER_ORIG_PATH"),
                WildMatch::new("BUNDLER_ORIG_RB_USER_INSTALL"),
                WildMatch::new("BUNDLER_ORIG_RUBYLIB"),
                WildMatch::new("BUNDLER_ORIG_RUBYOPT"),
                WildMatch::new("BUNDLER_VERSION"),
                WildMatch::new("BUNDLE_APP_CONFIG"),
                WildMatch::new("BUNDLE_BIN_PATH"),
                WildMatch::new("BUNDLE_FORCE_RUBY_PLATFORM"),
                WildMatch::new("BUNDLE_GEMFILE"),
                WildMatch::new("BUNDLE_GEM_PATH"),
                WildMatch::new("BUNDLE_PATH"),
                WildMatch::new("CLASSPATH"),
                WildMatch::new("GEM_HOME"),
                WildMatch::new("GEM_PATH"),
                WildMatch::new("HOME"),
                WildMatch::new("HOMEPATH"),
                WildMatch::new("JAVA_EXE"),
                WildMatch::new("JAVA_HOME"),
                WildMatch::new("PATH"),
                WildMatch::new("PWD"),
                WildMatch::new("PYTHONPATH"),
                WildMatch::new("RUBYLIB"),
                WildMatch::new("RUBYOPT"),
                WildMatch::new("RUST_LOG"),
                WildMatch::new("_JAVA_OPTIONS"),
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

/// Translate ToIter<AsRef<str>> of "K=V" to HashMap.
pub(crate) fn parse_raw_env<'a, S: AsRef<str> + 'a + ?Sized, T: IntoIterator<Item = &'a S>>(
    raw: T,
) -> HashMap<String, String> {
    raw.into_iter()
        .map(|key_and_value| key_and_value.as_ref().splitn(2, '=').collect::<Vec<_>>())
        // [["DB", "foo.db"], ["PORT", "99"], ["HOST"], ["PATH", "/fake"]]
        .filter_map(
            |mut keys_and_values| match (keys_and_values.pop(), keys_and_values.pop()) {
                (Some(value), Some(key)) => Some((key.to_string(), value.to_string())),
                _ => None,
            },
        )
        // [("DB", "foo.db")]
        .collect::<HashMap<_, _>>()
}

pub(crate) async fn get_proc_environ(path: PathBuf) -> Result<HashMap<String, String>> {
    let mut environ_file = tokio::fs::File::open(path).await?;

    let mut raw_env_vars = String::with_capacity(8192);

    // TODO: nginx doesn't play nice when we do this, it only returns a string that goes like
    // "nginx -g daemon off;".
    let _read_amount = environ_file.read_to_string(&mut raw_env_vars).await?;

    Ok(parse_raw_env(raw_env_vars.split_terminator(char::from(0))))
}

/// Helper function that loads the process' environment variables, and selects only those that were
/// requested from `mirrord-layer` (ignores vars specified in `filter_env_vars`).
///
/// NOTE: can remove `RemoteResult` when we break protocol compatibility.
#[tracing::instrument(level = "trace", skip(full_env))]
pub(crate) fn select_env_vars(
    full_env: &HashMap<String, String>,
    filter_env_vars: HashSet<String>,
    select_env_vars: HashSet<String>,
) -> RemoteResult<HashMap<String, String>> {
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
