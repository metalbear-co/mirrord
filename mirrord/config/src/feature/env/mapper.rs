use std::collections::HashMap;

use fancy_regex::Regex;
use tracing::Level;

use crate::config::ConfigError;

#[derive(Debug)]
pub struct EnvVarsRemapper {
    mapping: Vec<(Regex, String)>,
    env_vars: HashMap<String, String>,
}

impl EnvVarsRemapper {
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn new(
        mapping: HashMap<String, String>,
        env_vars: HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let mapping = mapping
            .into_iter()
            .map(|(pattern, value)| {
                Ok::<_, ConfigError>((
                    Regex::new(&pattern).map_err(|fail| ConfigError::Regex {
                        pattern,
                        value: value.clone(),
                        fail: Box::new(fail),
                    })?,
                    value,
                ))
            })
            .try_collect()?;

        Ok(EnvVarsRemapper { mapping, env_vars })
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn remapped(self) -> HashMap<String, String> {
        let Self {
            mapping,
            mut env_vars,
        } = self;

        env_vars.iter_mut().for_each(|(name, value)| {
            // Replaces the value inline.
            if let Some((_, replace_with)) = mapping
                .iter()
                .find(|(regex, _)| regex.is_match(name).unwrap_or(false))
                .cloned()
            {
                *value = replace_with;
            }
        });

        env_vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_vars() -> HashMap<String, String> {
        [
            ("Lech".to_string(), "Legendary".to_string()),
            ("Krakus_I".to_string(), "Legendary".to_string()),
            ("Leszko_I".to_string(), "Legendary".to_string()),
            ("Leszko_II".to_string(), "Legendary".to_string()),
            ("Leszko_III".to_string(), "Legendary".to_string()),
            ("Popiel_II".to_string(), "Legendary".to_string()),
            ("Piast_the_Wheelwright".to_string(), "Legendary".to_string()),
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
            (
                ".+zko_I$".to_string(),
                "Defeated the Hungarians".to_string(),
            ),
            (".+zko_II.*".to_string(), "Succession".to_string()),
            (
                "([[:alpha:]]|_+)+(Wheel.+)".to_string(),
                "Legendary founder of the Piast dinasty".to_string(),
            ),
        ]
        .into()
    }

    #[test]
    fn simple_mapping() {
        let mut remapper = EnvVarsRemapper::new(mappings(), env_vars())
            .unwrap()
            .remapped();

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

        assert_eq!(Some("Succession".to_string()), remapper.remove("Leszko_II"));

        assert_eq!(
            Some("Succession".to_string()),
            remapper.remove("Leszko_III")
        );

        assert_eq!(Some("Legendary".to_string()), remapper.remove("Popiel_II"));

        assert_eq!(
            Some("Legendary founder of the Piast dinasty".to_string()),
            remapper.remove("Piast_the_Wheelwright")
        );
    }

    #[test]
    #[should_panic]
    fn does_not_accept_invalid_regex() {
        let mut invalid_mapping = HashMap::new();
        invalid_mapping.insert("(".to_string(), "Not from Poland".to_string());

        EnvVarsRemapper::new(invalid_mapping, HashMap::new())
            .inspect_err(|fail| println!("{fail}"))
            .unwrap();
    }
}
