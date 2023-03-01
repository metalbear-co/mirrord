use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, Result},
    util::{MirrordToggleableConfig, VecOrSingle},
};

// TODO: update docs.
/// Specify which paths of tcp unix sockets should be connected to on the remote target and which
/// should be connected to locally.
/// Paths can be specified as regular expressions according to
/// [this syntax](https://docs.rs/regex/1.7.1/regex/index.html#syntax).
///
/// If you only want to specify a single path (or regular expression), you don't need to put it
/// inside an array, you can pass the string directly.
///
/// You can either specify the remote paths (allow list) or the local paths (block list), not both.
/// If you specify the remote paths, only the specified paths will be connected to remotely, and
/// all other paths will be connected to on your local system.
/// If you specify the local paths, all paths will be connected to remotely, except for the
/// specified paths.
///
/// ## Examples
///
/// ### Connect to all paths remotely by including them all with a regular expression
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network.outgoing.unix_tcp]
/// remote = ".*"
/// ```
///
/// ### Connect to all paths remotely by passing an empty local list
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network.outgoing.unix_tcp]
/// local = [] # Don't do anything locally - connect to all unix addresses remotely.
/// ```
///
/// ### Connect to all paths locally by completely disabling the unix_tcp option
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network.outgoing]
/// unix_tcp = false
/// ```
///
/// ### Connect to all paths remotely except for paths in /tmp/local-socket and paths that end with "-local"
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network.outgoing.unix_tcp]
/// local = [
///     "^/tmp/local-socket/.*",
///     "-local$",
/// ]
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "UnixTcpFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct UnixTcpConfig {
    /// Connect to these paths remotely (and to all other paths locally).
    ///
    /// Value is a list separated by ";".
    #[config(env = "MIRRORD_OUTGOING_UNIX_TCP_REMOTE_LIST")]
    pub remote_list: Option<VecOrSingle<String>>,
}

impl MirrordToggleableConfig for UnixTcpFileConfig {
    fn disabled_config() -> Result<Self::Generated> {
        let pattern_list = FromEnv::new("MIRRORD_OUTGOING_UNIX_TCP_REMOTE_LIST")
            .source_value()
            .transpose()?;

        Ok(UnixTcpConfig {
            remote_list: pattern_list,
        })
    }
}

// TODO
// #[cfg(test)]
// mod tests {
//     use rstest::rstest;
//
//     use super::*;
//     use crate::{
//         config::MirrordConfig,
//         util::{testing::with_env_vars, ToggleableConfig},
//     };
//
//     #[rstest]
//     fn default(
//         #[values((None, None), (Some("IVAR1"), Some("IVAR1")), (Some("IVAR1;IVAR2"),
// Some("IVAR1;IVAR2")))]         include: (Option<&str>, Option<&str>),
//         #[values((None, None), (Some("EVAR1"), Some("EVAR1")))] exclude: (
//             Option<&str>,
//             Option<&str>,
//         ),
//     ) {
//         with_env_vars(
//             vec![
//                 ("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE", include.0),
//                 ("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE", exclude.0),
//             ],
//             || {
//                 let env = EnvFileConfig::default().generate_config().unwrap();
//
//                 assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
//                 assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
//             },
//         );
//     }
//
//     #[rstest]
//     fn disabled_config(
//         #[values((None, None), (Some("IVAR1"), Some("IVAR1")))] include: (
//             Option<&str>,
//             Option<&str>,
//         ),
//         #[values((None, Some("*")), (Some("EVAR1"), Some("EVAR1")))] exclude: (
//             Option<&str>,
//             Option<&str>,
//         ),
//     ) {
//         with_env_vars(
//             vec![
//                 ("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE", include.0),
//                 ("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE", exclude.0),
//             ],
//             || {
//                 let env = ToggleableConfig::<EnvFileConfig>::Enabled(false)
//                     .generate_config()
//                     .unwrap();
//
//                 assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
//                 assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
//             },
//         );
//     }
// }
