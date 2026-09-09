//! Injection selection shared by CLI launches and intercepted child creation.

use std::{fmt, str::FromStr};

/// Forward the selected method to descendants created through layer hooks.
pub const MIRRORD_INJECTION_METHOD: &str = "MIRRORD_INJECTION_METHOD";

/// Explicit Windows injection methods; selection never falls back automatically.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InjectionMethod {
    /// Load immediately through a remote thread.
    #[default]
    LoadLibrary,
    /// Queue loading on the primary thread before application execution.
    Apc,
    /// Rewrite imports in a newly created, never-resumed process.
    Iat,
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

    /// Attach cannot establish the never-run-loader requirement for IAT.
    pub fn parse_attach(value: &str) -> Result<Self, String> {
        match value.parse()? {
            Self::Iat => Err(
                "attach supports load-library or apc; iat requires a newly created process"
                    .to_owned(),
            ),
            method => Ok(method),
        }
    }
}

impl FromStr for InjectionMethod {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "load-library" => Ok(Self::LoadLibrary),
            "apc" => Ok(Self::Apc),
            "iat" => Ok(Self::Iat),
            _ => Err(format!(
                "unknown injection method {value:?}; expected load-library, apc, or iat"
            )),
        }
    }
}

impl fmt::Display for InjectionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LoadLibrary => "load-library",
            Self::Apc => "apc",
            Self::Iat => "iat",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_strings_are_explicit_and_attach_rejects_iat() {
        for method in [
            InjectionMethod::LoadLibrary,
            InjectionMethod::Apc,
            InjectionMethod::Iat,
        ] {
            assert_eq!(
                method.to_string().parse::<InjectionMethod>().unwrap(),
                method
            );
        }
        assert!(InjectionMethod::parse_attach("iat").is_err());
        assert!(InjectionMethod::parse_attach("apc").is_ok());
        assert!("auto".parse::<InjectionMethod>().is_err());
    }
}
