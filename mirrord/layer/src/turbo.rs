//! "turbo" magic: a heuristic workaround for Next.js + Turbopack under mirrord.
//!
//! Turbopack runs transforms (e.g. PostCSS for CSS) in pooled Node worker subprocesses, using a
//! loopback TCP channel for parent<->worker IPC: the parent listens on `127.0.0.1:0` and the
//! spawned worker connects back to that port. With the mirrord layer loaded into the worker, that
//! loopback `connect` is otherwise routed through the agent (remote), so the worker never reaches
//! the local parent, exits, and the build fails (e.g. `next dev` crashes compiling CSS).
//!
//! When we detect that the layer was loaded into a Next.js process we enable
//! [`OutgoingConfig::ignore_localhost`](mirrord_config::feature::network::outgoing::OutgoingConfig),
//! which keeps loopback connections local.
//!
//! The decision has to reach the Turbopack worker children, which don't look like `next`
//! themselves. We can't mutate the children's config directly (they decode the same pre-baked
//! `MIRRORD_RESOLVED_CONFIG` blob the CLI produced), so we propagate via an environment variable:
//! [`detect`] sets [`IGNORE_LOCALHOST_ENV`] on the first (top-level) `next` process, the children
//! inherit it, and every process applies it to its own decoded config in [`apply`].

use std::ffi::OsString;

use mirrord_config::LayerConfig;

/// Environment variable used to both signal and propagate the turbo workaround to child processes.
const IGNORE_LOCALHOST_ENV: &str = "MIRRORD_OUTGOING_IGNORE_LOCALHOST";

/// Detects a Next.js process from its `args` and, if found, sets [`IGNORE_LOCALHOST_ENV`] so that
/// this process and all of its children enable localhost-ignoring for outgoing connections.
///
/// Must be called before [`apply`] (which reads the variable) and from single-threaded code (the
/// layer constructor), as it mutates the environment.
pub(crate) fn detect(args: &[OsString]) {
    if std::env::var_os(IGNORE_LOCALHOST_ENV).is_some() {
        // Already set - by the user, or inherited from a parent Next.js process. Nothing to do.
        return;
    }

    if is_next_process(args) {
        tracing::debug!("turbo: detected a Next.js process, enabling outgoing.ignore_localhost");
        // SAFETY: called from the single-threaded layer constructor, before any threads start.
        unsafe { std::env::set_var(IGNORE_LOCALHOST_ENV, "true") };
    }
}

/// Applies the turbo workaround to `config` when [`IGNORE_LOCALHOST_ENV`] is set - either by
/// [`detect`] in this process, or inherited from a parent Next.js process.
pub(crate) fn apply(config: &mut LayerConfig) {
    if std::env::var_os(IGNORE_LOCALHOST_ENV).is_some() {
        config.feature.network.outgoing.ignore_localhost = true;
    }
}

/// Heuristic: does this look like a Next.js CLI/server process?
///
/// Matches the `next` launcher in its common forms, e.g. `node .../node_modules/.bin/next dev` or
/// `node .../next/dist/bin/next start`.
fn is_next_process(args: &[OsString]) -> bool {
    args.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        arg == "next" || arg.ends_with("/next") || arg.contains("/next/dist/")
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use rstest::rstest;

    use super::is_next_process;

    fn args(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[rstest]
    #[case(&["node", "/app/node_modules/.bin/next", "dev"])]
    #[case(&["node", "/app/node_modules/next/dist/bin/next", "start"])]
    #[case(&["next", "dev"])]
    fn detects_next(#[case] parts: &[&str]) {
        assert!(is_next_process(&args(parts)));
    }

    #[rstest]
    #[case(&["node", "/app/server.js"])]
    #[case(&["node", "/app/dist/bin/nextish", "run"])]
    #[case(&["/bin/bash", "-c", "echo next"])]
    fn ignores_non_next(#[case] parts: &[&str]) {
        assert!(!is_next_process(&args(parts)));
    }
}
