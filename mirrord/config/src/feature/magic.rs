use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

pub(crate) const JAVA_OPTIONS_ENV: &str = "JDK_JAVA_OPTIONS";

/// Sensible default behaviors that help most users. Disable individual flags only if they conflict
/// with your setup.
///
/// ```json
/// {
///   "feature": {
///     "magic": {
///       "aws": true
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "MagicFileConfig", derive = "JsonSchema, Serialize")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct MagicConfig {
    /// #### feature.magic.java_agents {#feature-magic-java_agents}
    ///
    /// Remote Java agents often refer to files that do not exist on the local machine. When
    /// enabled, mirrord removes `-javaagent:` options from the remote `JDK_JAVA_OPTIONS` value
    /// before it starts the local process.
    ///
    /// Disable this only if the remote Java agent is also available locally.
    ///
    /// Defaults to `true`.
    #[config(default = true)]
    pub java_agents: bool,

    /// #### feature.magic.aws {#feature-magic-aws}
    ///
    /// The AWS CLI prefers local credentials (e.g. `~/.aws`, `AWS_PROFILE`) over the remote pod's
    /// identity (IAM role, instance profile, IRSA). When those local credentials are present, the
    /// pod's own identity is never used, which is rarely what you want in a mirrord session.
    ///
    /// When enabled, mirrord makes local AWS configuration unavailable to the process by:
    /// - Unsetting `AWS_PROFILE` and related AWS environment variables.
    /// - Mapping `~/.aws` to a temporary directory, so the AWS CLI cannot read local credentials
    ///   and also has a writable location for its credential cache (avoiding errors on cache
    ///   writes).
    ///
    /// This allows the remote pod's IAM role / instance profile to be used as intended.
    ///
    /// Disable this only if you intentionally need local AWS credentials inside the local mirrord'
    /// process.
    ///
    /// Defaults to `true`.
    #[config(default = true)]
    pub aws: bool,

    /// #### feature.magic.auto_mount {#feature-magic-auto_mount}
    ///
    /// Kubernetes mounts ConfigMaps, Secrets, and volumes (e.g. PVCs) into the target container at
    /// fixed paths. When mirrord runs your process locally, those paths either don't exist or hold
    /// unrelated local data, so the process can't read the configuration or secrets it expects
    /// from the pod.
    ///
    /// When enabled, mirrord reads the target container's volume mounts from the remote pod spec
    /// and adds their mount paths to [`feature.fs.read_only`](#feature-fs-read_only), so the
    /// local process transparently reads those files from the remote pod while still writing
    /// locally.
    ///
    /// This matters when the file system mode is `localwithoverrides`: the mounted paths would
    /// otherwise be served locally and not exist, so `auto_mount` forces them to be read from the
    /// remote pod. In the default `read` mode files are already read remotely, and in fully
    /// `local` mode all overrides are ignored, so `auto_mount` has no observable effect in
    /// either of those modes.
    ///
    /// Paths you've explicitly marked local via `feature.fs.local` are left untouched.
    ///
    /// Disable this only if you intentionally want the mounted paths served from the local file
    /// system.
    ///
    /// Defaults to `true`.
    #[config(default = true)]
    pub auto_mount: bool,

    /// #### feature.magic.turbo {#feature-magic-turbo}
    ///
    /// Next.js + Turbopack runs transforms (e.g. PostCSS for CSS) in pooled Node worker
    /// subprocesses, which talk to the parent over a loopback TCP channel (the parent listens on
    /// `127.0.0.1:0` and the worker connects back). With the mirrord layer loaded into the worker,
    /// that loopback connection is routed through the agent (remote) by default, so the worker
    /// never reaches the local parent and the build fails (e.g. `next dev` crashes compiling CSS).
    ///
    /// When enabled, mirrord detects Next.js processes and keeps their loopback IPC local by
    /// turning on
    /// [`feature.network.outgoing.ignore_localhost`](#feature-network-outgoing-ignore_localhost)
    /// for the Next.js process and the Turbopack workers it spawns.
    ///
    /// Disable this only if you intentionally need outgoing localhost connections from a Next.js
    /// process to be routed to the remote pod.
    ///
    /// Defaults to `true`.
    #[config(default = true)]
    pub turbo: bool,
}

/// Removes Java agents from [`JAVA_OPTIONS_ENV`] while preserving the original spelling of every
/// other launcher argument.
///
/// Java separates these arguments on ASCII whitespace and removes matching quote pairs. Backslash
/// and `#` have no special meaning, so a shell command parser cannot safely process this value.
pub(crate) fn without_java_agents(options: &str) -> Option<String> {
    const JAVA_AGENT_PREFIX: &str = "-javaagent:";

    // quotes can splice the prefix across quoted and unquoted fragments
    // this means we have to scan the whole thing if the value contains quotes
    if !options.contains(JAVA_AGENT_PREFIX) && !options.contains(['\'', '"']) {
        return None;
    }

    let mut sanitised_options = Vec::new();
    let mut token_start = None;
    let mut unquoted = String::new();
    let mut quote = None;
    let mut modified = false;

    // split using the Java launcher's quote rules
    options.char_indices().for_each(|(index, character)| {
        if let Some(expected) = quote {
            if character == expected {
                quote = None;
            } else {
                unquoted.push(character);
            }
        } else {
            match character {
                '\'' | '"' => {
                    token_start.get_or_insert(index);
                    quote = Some(character);
                }
                _ if character.is_ascii_whitespace() => {
                    // only split whitespace if we're not in a quoted section
                    if let Some(start) = token_start.take() {
                        if !unquoted.starts_with(JAVA_AGENT_PREFIX) {
                            sanitised_options.push(&options[start..index]);
                        } else {
                            modified = true;
                        }
                        unquoted.clear();
                    }
                }
                _ => {
                    token_start.get_or_insert(index);
                    unquoted.push(character);
                }
            }
        }
    });

    // we entered a quoted section but never left, so this env is malformed and we leave it alone
    if quote.is_some() {
        return None;
    }

    // push the final option
    if let Some(start) = token_start {
        if !unquoted.starts_with(JAVA_AGENT_PREFIX) {
            sanitised_options.push(&options[start..]);
        } else {
            modified = true;
        }
    }

    // return a modified value only if we removed some option(s)
    let sanitised_value = sanitised_options.join(" ");
    (modified).then_some(sanitised_value)
}

impl CollectAnalytics for &MagicConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("java_agents", self.java_agents);
        analytics.add("aws", self.aws);
        analytics.add("auto_mount", self.auto_mount);
        analytics.add("turbo", self.turbo);
    }
}
