use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use mirrord_protocol::RemoteResult;
use tokio::io::AsyncReadExt;
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
            let mut exclude = vec![WildMatch::new("PATH"), WildMatch::new("HOME")];

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
/// Returns an error if none of the requested environment variables were found.
pub async fn select_env_vars(
    environ_path: PathBuf,
    filter_env_vars: HashSet<String>,
    select_env_vars: HashSet<String>,
) -> RemoteResult<HashMap<String, String>> {
    trace!(
        "select_env_vars -> environ_path {:#?} filter_env_vars {:#?} select_env_vars {:#?}",
        environ_path,
        filter_env_vars,
        select_env_vars
    );

    let mut environ_file = tokio::fs::File::open(environ_path).await?;

    let mut raw_env_vars = String::with_capacity(8192);

    // TODO: nginx doesn't play nice when we do this, it only returns a string that goes like
    // "nginx -g daemon off;".
    let _read_amount = environ_file.read_to_string(&mut raw_env_vars).await?;

    let env_filter = EnvFilter::new(select_env_vars, filter_env_vars);

    let env_vars = raw_env_vars
        // "DB=foo.db\0PORT=99\0HOST=\0PATH=/fake\0"
        .split_terminator(char::from(0))
        // ["DB=foo.db", "PORT=99", "HOST=", "PATH=/fake"]
        .map(|key_and_value| key_and_value.splitn(2, '=').collect::<Vec<_>>())
        // [["DB", "foo.db"], ["PORT", "99"], ["HOST"], ["PATH", "/fake"]]
        .filter_map(
            |mut keys_and_values| match (keys_and_values.pop(), keys_and_values.pop()) {
                (Some(value), Some(key)) => Some((key.to_string(), value.to_string())),
                _ => None,
            },
        )
        .filter(|(key, _)| env_filter.matches(key))
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
