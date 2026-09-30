//! Injection selection shared by CLI launches and intercepted child creation.

use strum_macros::{Display, EnumString};

/// Forward the selected method to descendants created through layer hooks.
pub const MIRRORD_INJECTION_METHOD_ENV: &str = "MIRRORD_INJECTION_METHOD";

/// Explicit Windows injection methods; selection never falls back automatically.
///
/// Parsing ignores ASCII case, because the value often comes from an environment variable a person
/// typed; [`Display`](std::fmt::Display) always writes the canonical lowercase name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, EnumString, Display)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum InjectionMethod {
    /// Load immediately through a remote thread.
    #[default]
    LoadLibrary,
    /// Queue loading on the primary thread, which runs it before the executable's entry point.
    Apc,
    /// Rewrite imports in a newly created, never-resumed process.
    Iat,
}

/// A method name that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum InjectionMethodError {
    #[error("unknown injection method {0:?}; expected load-library, apc or iat")]
    Unknown(String),
    #[error("attach supports load-library or apc; iat requires a newly created process")]
    IatOnAttach,
}

impl InjectionMethod {
    /// Construct the injector corresponding to the selected method.
    pub fn injector(self) -> stork::Injector {
        match self {
            Self::LoadLibrary => stork::Injector::new(),
            Self::Apc => stork::Injector::queue_apc(),
            Self::Iat => stork::Injector::import_table(),
        }
    }

    /// Parses a method name, with an error that lists the names it accepts.
    pub fn parse(value: &str) -> Result<Self, InjectionMethodError> {
        value
            .parse()
            .map_err(|_| InjectionMethodError::Unknown(value.to_owned()))
    }

    /// Attach cannot establish the never-run-loader requirement for IAT.
    pub fn parse_attach(value: &str) -> Result<Self, InjectionMethodError> {
        match Self::parse(value)? {
            Self::Iat => Err(InjectionMethodError::IatOnAttach),
            method => Ok(method),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_method_names_the_choices() {
        let error = InjectionMethod::parse("auto").unwrap_err();
        assert_eq!(
            error.to_string(),
            r#"unknown injection method "auto"; expected load-library, apc or iat"#
        );
        assert!(matches!(
            InjectionMethod::parse_attach("IAT"),
            Err(InjectionMethodError::IatOnAttach)
        ));
    }
}
