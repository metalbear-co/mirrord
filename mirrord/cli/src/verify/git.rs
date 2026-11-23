use std::ops::Not;

use mirrord_command::resolve_tokio_command;
use url::Url;

use super::error::VerifyError;

/// What `mirrord verify run` records about the checkout it runs in.
#[derive(Debug)]
pub(super) struct GitFacts {
    pub repository: String,
    pub commit: String,
    pub tree: String,
    pub dirty: bool,
    pub branch: Option<String>,
}

/// Reads the state of the git checkout in the current directory.
pub(super) async fn git_facts() -> Result<GitFacts, VerifyError> {
    let commit = git(&["rev-parse", "HEAD"]).await?;
    let tree = git(&["rev-parse", "HEAD^{tree}"]).await?;
    let dirty = git(&["status", "--porcelain"]).await?.is_empty().not();
    let repository = git(&["config", "--get", "remote.origin.url"])
        .await
        .map(|url| without_credentials(&url))
        .unwrap_or_default();
    let branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok();

    Ok(GitFacts {
        repository,
        commit,
        tree,
        dirty,
        branch,
    })
}

async fn git(args: &[&str]) -> Result<String, VerifyError> {
    let output = resolve_tokio_command("git")
        .args(args)
        .output()
        .await
        .map_err(VerifyError::GitNotRun)?;

    if output.status.success().not() {
        return Err(VerifyError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A remote URL without a user name, a password or a trailing `.git`.
fn without_credentials(url: &str) -> String {
    let url = url.strip_suffix(".git").unwrap_or(url);

    match Url::parse(url) {
        Ok(mut parsed) if parsed.has_host() => {
            // `set_username` and `set_password` only fail for URLs without a host.
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => url.to_owned(),
    }
}
