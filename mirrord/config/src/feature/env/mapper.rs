use std::collections::HashMap;

use fancy_regex::Regex;
use tracing::Level;

#[derive(Debug)]
pub struct EnvVarsRemapper {
    mapping: Vec<(Regex, String)>,
    env_vars: HashMap<String, String>,
}

impl EnvVarsRemapper {
    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn new(mapping: HashMap<String, String>, env_vars: HashMap<String, String>) -> Self {
        let mapping = mapping
            .into_iter()
            .map(|(pattern, value)| {
                (
                    Regex::new(&pattern).expect("Building env vars mapping regex failed"),
                    value,
                )
            })
            .collect();

        EnvVarsRemapper { mapping, env_vars }
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn remapped(self) -> HashMap<String, String> {
        let Self { mapping, env_vars } = self;

        env_vars
            .into_iter()
            .fold(HashMap::with_capacity(32), |mut acc, (key, value)| {
                match mapping
                    .iter()
                    .find(|(k, _)| k.is_match(&key).ok().is_some_and(|matched| matched))
                    .map(|(_, to_value)| to_value.clone())
                {
                    Some(to_value) => acc.insert(key, to_value),
                    None => acc.insert(key, value),
                };

                acc
            })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn env_vars() -> HashMap<String, String> {
        [
            ("Lech".to_string(), "Legendary".to_string()),
            ("Krakus_I".to_string(), "Legendary".to_string()),
            ("Leszko_I".to_string(), "Legendary".to_string()),
            ("Popiel_II".to_string(), "Legendary".to_string()),
        ]
        .into()
    }

    fn mappings() -> HashMap<String, String> {
        [
            (
                "Lec.+".to_string(),
                "Legendary founder of the great Polish nation".to_string(),
            ),
            (
                "Krak.+".to_string(),
                "Legendary founder of Krakow".to_string(),
            ),
            (".+zko_I".to_string(), "Defeated the Hungarians".to_string()),
        ]
        .into()
    }

    #[rstest]
    fn simple_mapping() {
        let mut remapper = EnvVarsRemapper::new(mappings(), env_vars()).remapped();

        assert_eq!(
            Some("Legendary founder of the great Polish nation".to_string()),
            remapper.remove("Lech")
        );

        assert_eq!(
            Some("Legendary founder of Krakow".to_string()),
            remapper.remove("Krakus_I")
        );

        assert_eq!(
            Some("Defeated the Hungarians".to_string()),
            remapper.remove("Leszko_I")
        );

        assert_eq!(Some("Legendary".to_string()), remapper.remove("Popiel_II"));
    }
}
