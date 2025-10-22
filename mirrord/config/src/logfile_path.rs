use std::{
    borrow::Cow,
    fmt,
    hash::{Hash, Hasher},
    io,
    marker::PhantomData,
    ops::Deref,
    path::{Path, PathBuf},
    time::SystemTime,
};

use rand::distr::{Alphanumeric, SampleString};
use schemars::{JsonSchema, SchemaGenerator, schema::Schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::config::{ConfigContext, ConfigError, FromMirrordConfig, MirrordConfig};

/// Log source, e.g. internal proxy.
pub trait LogSource {
    /// Generates a randomized log file name.
    fn generate_file_name() -> String;
}

pub struct Intproxy;

impl LogSource for Intproxy {
    fn generate_file_name() -> String {
        let random_name: String = Alphanumeric.sample_string(&mut rand::rng(), 7);
        let timestamp = SystemTime::UNIX_EPOCH
            .elapsed()
            .expect("system time should not be earlier than UNIX EPOCH")
            .as_secs();
        format!("mirrord-intproxy-{timestamp}-{random_name}.log")
    }
}

pub struct Extproxy;

impl LogSource for Extproxy {
    fn generate_file_name() -> String {
        let random_name: String = Alphanumeric.sample_string(&mut rand::rng(), 7);
        let timestamp = SystemTime::UNIX_EPOCH
            .elapsed()
            .expect("system time should not be earlier than UNIX EPOCH")
            .as_secs();
        format!("mirrord-extproxy-{timestamp}-{random_name}.log")
    }
}

/// Path to log destination.
///
/// Can be specified in the config as either:
/// 1. Path to an exact log file.
/// 2. Path to a directory where the log file should be created.
///
/// This is checked at runtime, based on the value provided by the user:
/// 1. If the path ends with a slash, assume it points to a directory.
/// 2. Otherwise, if the path exists, check if it's a directory or not.
/// 3. Otherwise, assume it points to a file.
///
/// During generation ([`MirrordConfig::generate_config`]), the provided value is always resolved to
/// the final file.
///
/// Defaults to a path in the temporary directory.
///
/// **Important:** when using this as a field, always annotate it with `#[config(nested)]`.
#[derive(Eq)]
pub struct LogDestinationConfig<S> {
    pub path: PathBuf,
    _phantom: PhantomData<fn() -> S>,
}

impl<S> Clone for LogDestinationConfig<S> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            _phantom: Default::default(),
        }
    }
}

impl<S: LogSource> Default for LogDestinationConfig<S> {
    fn default() -> Self {
        let mut path = std::env::temp_dir();
        path.push(S::generate_file_name());

        Self {
            path,
            _phantom: Default::default(),
        }
    }
}

impl<S> fmt::Debug for LogDestinationConfig<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path.fmt(f)
    }
}

impl<S> PartialEq for LogDestinationConfig<S> {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl<S> Hash for LogDestinationConfig<S> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

impl<S> JsonSchema for LogDestinationConfig<S> {
    fn is_referenceable() -> bool {
        PathBuf::is_referenceable()
    }

    fn schema_name() -> String {
        PathBuf::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        PathBuf::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        PathBuf::json_schema(generator)
    }
}

impl<S: LogSource> MirrordConfig for LogDestinationConfig<S> {
    type Generated = LogDestinationConfig<S>;

    fn generate_config(mut self, _: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let exists = match self.path.try_exists() {
            Ok(exists) => exists,
            Err(error) => {
                return Err(ConfigError::FileAccessFailed {
                    path: self.path,
                    error,
                });
            }
        };

        let is_dir = self.path.is_dir();
        let has_trailing_slash = self
            .path
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR);
        let push_name = match (exists, is_dir, has_trailing_slash) {
            (false, _, true) => true,
            (false, _, false) => false,
            (true, true, _) => true,
            (true, false, false) => false,
            (true, false, true) => {
                return Err(ConfigError::FileAccessFailed {
                    path: self.path.clone(),
                    error: io::ErrorKind::NotADirectory.into(),
                });
            }
        };

        if push_name {
            self.path.push(S::generate_file_name());
        }

        Ok(self)
    }
}

impl<S: LogSource> FromMirrordConfig for LogDestinationConfig<S> {
    type Generator = LogDestinationConfig<S>;
}

impl<S> Serialize for LogDestinationConfig<S> {
    fn serialize<SE>(&self, serializer: SE) -> Result<SE::Ok, SE::Error>
    where
        SE: Serializer,
    {
        self.path.serialize(serializer)
    }
}

impl<'de, S> Deserialize<'de> for LogDestinationConfig<S> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        PathBuf::deserialize(deserializer).map(|path| Self {
            path,
            _phantom: Default::default(),
        })
    }
}

impl<S> Deref for LogDestinationConfig<S> {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl<S> AsRef<Path> for LogDestinationConfig<S> {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod test {
    use mirrord_config_derive::MirrordConfig;
    use rstest::rstest;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    use crate::{
        config::MirrordConfig,
        logfile_path::{LogDestinationConfig, LogSource},
    };

    struct TestSource;

    impl LogSource for TestSource {
        fn generate_file_name() -> String {
            "test.log".into()
        }
    }

    #[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
    #[config(map_to = "TestFileConfig", derive = "JsonSchema")]
    struct TestConfig {
        #[config(default, nested)]
        log_destination: LogDestinationConfig<TestSource>,
    }

    #[test]
    fn has_default() {
        let raw = serde_json::Value::Object(Default::default());
        let config = serde_json::from_value::<TestFileConfig>(raw)
            .unwrap()
            .generate_config(&mut Default::default())
            .unwrap();
        assert!(config.log_destination.ends_with("test.log"));
    }

    fn test_tmpdir() -> TempDir {
        let tmpdir = tempfile::tempdir().unwrap();
        std::fs::write(tmpdir.path().join("file"), "").unwrap();
        std::fs::create_dir(tmpdir.path().join("dir")).unwrap();
        tmpdir
    }

    /// Verifies that [`LogDestinationConfig`] generates properly as a nested config.
    #[rstest]
    // Input path has no trailing slash, exists, and is a file.
    // Config should resolve to that path.
    #[case::existing_file("file", Some("file"))]
    // Input path has a trailing slash, exists, and is a file.
    // Config resolution should fail - file used as a directory.
    #[case::existing_file_used_as_dir("file/", None)]
    // Input path has no trailing slash, exists, and is a directory.
    // Config should resolve to a path within that directory.
    #[case::existing_dir_no_slash("dir", Some("dir/test.log"))]
    // Input path has a trailing slash, exists, and is a directory.
    // Config should resolve to a path within that directory.
    #[case::existing_dir_slash("dir/", Some("dir/test.log"))]
    // Input path has no trailing slash, and does not exist.
    // Config should resolve to that path.
    #[case::new_file_no_slash("new", Some("new"))]
    // Input path has a trailing slash, and does not exist.
    // Config should resolve to a path within that directory.
    #[case::new_dir_slash("new/", Some("new/test.log"))]
    #[test]
    fn verify_nested_behavior(#[case] input: &str, #[case] expected: Option<&str>) {
        let tmpdir = test_tmpdir();
        let raw = serde_json::json!({
            "log_destination": format!("{}/{input}", tmpdir.path().display()),
        });
        let config_result = serde_json::from_value::<TestFileConfig>(raw)
            .unwrap()
            .generate_config(&mut Default::default());
        match (config_result, expected) {
            (Ok(config), Some(expected)) => {
                assert_eq!(
                    config
                        .log_destination
                        .strip_prefix(tmpdir.path())
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    expected,
                );

                std::fs::create_dir_all(config.log_destination.parent().unwrap()).unwrap();
                std::fs::write(&config.log_destination, "hello").unwrap();
            }
            (Ok(config), None) => {
                panic!("generating config should fail: config={config:?}");
            }
            (Err(error), Some(expected)) => {
                panic!("generating config should not fail: error={error}, expected={expected}")
            }
            (Err(..), None) => {}
        }
    }
}
