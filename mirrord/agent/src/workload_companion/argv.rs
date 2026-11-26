//! The application's command line, which the workload companion serves as
//! [`APP_ARGV_ENV`](mirrord_remote_layer_protocol::APP_ARGV_ENV), so that a developer can start a
//! local process with the task's own arguments.
//!
//! The arguments can carry secrets, so nothing here logs them.

use mirrord_remote_layer_protocol::{APP_ARGV_MAX_LEN, APP_PID_ENV};

/// The command line of the application whose PID the bootstrap set in [`APP_PID_ENV`], as a JSON
/// array of strings, with bytes that aren't UTF-8 replaced.
///
/// [`None`] when the PID is missing or invalid, for example under a bootstrap that doesn't set
/// it, when its command line can't be read, or when the JSON is longer than [`APP_ARGV_MAX_LEN`].
pub(crate) async fn application_argv() -> Option<String> {
    let Some(pid) = std::env::var(APP_PID_ENV)
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
    else {
        tracing::debug!(
            "No valid application PID in MIRRORD_REMOTE_APP_PID, so no application arguments are \
             served"
        );
        return None;
    };

    let cmdline = match tokio::fs::read(format!("/proc/{pid}/cmdline")).await {
        Ok(cmdline) if !cmdline.is_empty() => cmdline,
        // A zombie process has an empty command line.
        Ok(..) => {
            tracing::debug!(
                pid,
                "The application's command line is empty, so no application arguments are served"
            );
            return None;
        }
        Err(error) => {
            tracing::debug!(
                pid,
                %error,
                "Failed to read the application's command line, so no application arguments are \
                 served"
            );
            return None;
        }
    };

    // Each argument ends with a NUL. A process that rewrote its command line may have dropped
    // the last one.
    let cmdline = cmdline.strip_suffix(&[0]).unwrap_or(&cmdline);
    let argv = cmdline
        .split(|byte| *byte == 0)
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>();

    // serde_json fails only on maps with keys that aren't strings, and on failing `Serialize`
    // impls, neither of which a list of strings has.
    let argv = serde_json::to_string(&argv).expect("a list of strings serializes to JSON");
    if argv.len() > APP_ARGV_MAX_LEN {
        tracing::debug!(
            len = argv.len(),
            max_len = APP_ARGV_MAX_LEN,
            "The application's command line is too long, so no application arguments are served"
        );
        return None;
    }

    Some(argv)
}
