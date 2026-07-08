use std::collections::HashMap;

use fancy_regex::Regex;
use tracing::Level;

use crate::config::ConfigError;

/// Maps an env var found in `env_vars` that matches the regexes specified in `mapping`,
/// to the user specified value.
///
/// In other words: if we have an env var `BEST_LAND=ENGLAND` in `env_vars`, and there's
/// a [`Regex`] string pair in `mapping` `(".+LAND", "POLAND")`, then we override the var,
/// resulting in `BEST_LAND=POLAND`.
#[derive(Debug)]
pub struct EnvVarsRemapper {
    mapping: Vec<(Regex, String)>,
    env_vars: HashMap<String, String>,
}

impl EnvVarsRemapper {
    /// Converts `mapping` into a pair of [`Regex`], `String`.
    ///
    /// - `env_vars`: We take ownership of the env vars that were loaded somewhere else
    /// (for example, from `fetch_env_vars`).
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

    /// Does the actual mapping of env vars explained in [`EnvVarsRemapper`].
    ///
    /// - Returns the `HashMap` of all the env vars that were passed to [`Self::new`], even
    /// if the ones that did not match any mapping regex.
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
            ("Lech".to_owned(), "Legendary".to_owned()),
            ("Krakus_I".to_owned(), "Legendary".to_owned()),
            ("Leszko_I".to_owned(), "Legendary".to_owned()),
            ("Leszko_II".to_owned(), "Legendary".to_owned()),
            ("Leszko_III".to_owned(), "Legendary".to_owned()),
            ("Popiel_II".to_owned(), "Legendary".to_owned()),
            ("Piast_the_Wheelwright".to_owned(), "Legendary".to_owned()),
        ]
        .into()
    }

    fn mappings() -> HashMap<String, String> {
        [
            (
                "Lec.+".to_owned(),
                "Legendary founder of the great Polish nation".to_owned(),
            ),
            (
                "Krak.+".to_owned(),
                "Legendary founder of Krakow".to_owned(),
            ),
            (".+zko_I$".to_owned(), "Defeated the Hungarians".to_owned()),
            (".+zko_II.*".to_owned(), "Succession".to_owned()),
            (
                "([[:alpha:]]|_+)+(Wheel.+)".to_owned(),
                "Legendary founder of the Piast dinasty".to_owned(),
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
            Some("Legendary founder of the great Polish nation".to_owned()),
            remapper.remove("Lech")
        );

        assert_eq!(
            Some("Legendary founder of Krakow".to_owned()),
            remapper.remove("Krakus_I")
        );

        assert_eq!(
            Some("Defeated the Hungarians".to_owned()),
            remapper.remove("Leszko_I")
        );

        assert_eq!(Some("Succession".to_owned()), remapper.remove("Leszko_II"));

        assert_eq!(Some("Succession".to_owned()), remapper.remove("Leszko_III"));

        assert_eq!(Some("Legendary".to_owned()), remapper.remove("Popiel_II"));

        assert_eq!(
            Some("Legendary founder of the Piast dinasty".to_owned()),
            remapper.remove("Piast_the_Wheelwright")
        );
    }

    #[test]
    #[should_panic]
    fn does_not_accept_invalid_regex() {
        let mut invalid_mapping = HashMap::new();
        invalid_mapping.insert("(".to_owned(), "Not from Poland".to_owned());

        EnvVarsRemapper::new(invalid_mapping, HashMap::new())
            .inspect_err(|fail| println!("{fail}"))
            .unwrap();
    }
}
