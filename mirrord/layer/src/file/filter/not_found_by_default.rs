/// When running in a mode other than `local`, mirrord will make these paths under the running
/// user's home directory be not found, unless they are covered by patterns in the
/// `feature.fs.read_only` or `feature.fs.read_write` pattern sets (and not in the
/// `feature.fs.not_found` set).
pub const PATHS: [&str; 4] = [r"\.aws", r"\.config/gcloud", r"\.kube", r"\.azure"];
