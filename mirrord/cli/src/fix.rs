use std::{ffi::OsString, path::PathBuf};

use kube::config::{Kubeconfig, KubeconfigError};
use mirrord_progress::{Progress, ProgressTracker};
use serde_yaml::Value;
use yamlpatch::{Op, Patch, apply_yaml_patches};
use yamlpath::{Document, route};

use crate::{
    CliResult,
    config::{FixArgs, FixCommand, FixKubeconfig},
};

pub async fn fix_command(args: FixArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord fix");
    match args.command {
        FixCommand::Kubeconfig(args) => fix_kubeconfig(args, &mut progress).await?,
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum FixKubeconfigError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("error reading kubeconfig")]
    Kubeconfig(#[from] KubeconfigError),

    #[error("error patching yaml")]
    YamlPatch(#[from] yamlpatch::Error),

    #[error("error parsing yaml")]
    YamlQuery(#[from] yamlpath::QueryError),

    #[error("failed to fetch the user's home directory")]
    Home,

    #[error("absolute path {0:?} contains non-unicode characters")]
    StringConversion(OsString),

    #[error("there is no `current-context` field in kubeconfig")]
    NoCurrentCtx,

    #[error("`current-context` ({0:?}) refers to a nonexistent context")]
    MissingCtx(String),

    #[error("currently selected context {0:?} is malformed")]
    MalformedCtx(String),

    #[error("currently selected context {ctx:?} mentions user {user:?}, which does not exist")]
    MissingUser { ctx: String, user: String },

    #[error(
        "user {user}'s auth config uses {cmd_path:?}, which cannot be resolved using the current $PATH ({err})"
    )]
    CannotResolve {
        user: String,
        cmd_path: PathBuf,
        err: which::Error,
    },
}

async fn fix_kubeconfig<P: Progress>(
    args: FixKubeconfig,
    progress: &mut P,
) -> Result<(), FixKubeconfigError> {
    let kubeconfig_path = args.file_path.unwrap_or(
        std::env::home_dir()
            .ok_or(FixKubeconfigError::Home)?
            .join(".kube/config"),
    );
    let config = Kubeconfig::read_from(&kubeconfig_path)?;

    let current_ctx_name = config
        .current_context
        .ok_or(FixKubeconfigError::NoCurrentCtx)?;

    let current_ctx_entry = config
        .contexts
        .into_iter()
        .find(|ctx| ctx.name == current_ctx_name)
        .ok_or_else(|| FixKubeconfigError::MissingCtx(current_ctx_name.clone()))?;

    let current_ctx = current_ctx_entry
        .context
        .ok_or_else(|| FixKubeconfigError::MalformedCtx(current_ctx_name.clone()))?;

    let Some(current_user_name) = current_ctx.user else {
        progress.warning("Currently selected context has no user credentials");
        return Ok(());
    };

    let (current_user_idx, current_user) = config
        .auth_infos
        .into_iter()
        .enumerate()
        .find(|(_, u)| u.name == *current_user_name)
        .ok_or_else(|| FixKubeconfigError::MissingUser {
            ctx: current_ctx_name.clone(),
            user: current_user_name.clone(),
        })?;

    let command: Option<_> = try { current_user.auth_info?.exec?.command? };

    let command = match command {
        Some(cmd) => cmd,
        None => {
            progress.info("Currently selected user has no `exec` command, nothing to do.");
            return Ok(());
        }
    };

    let cmd_path = PathBuf::from(command);

    if cmd_path.is_absolute() {
        progress.info("Currently selected user's `exec` command already uses an absolute path.");
        return Ok(());
    }

    let absolute = which::which(&cmd_path)
        .map_err(|err| FixKubeconfigError::CannotResolve {
            user: current_user_name.clone(),
            cmd_path: cmd_path.clone(),
            err,
        })?
        .into_os_string()
        .into_string()
        .map_err(FixKubeconfigError::StringConversion)?;

    let document = Document::new(tokio::fs::read_to_string(&kubeconfig_path).await?)?;

    let patch = Patch {
        route: route!("users", current_user_idx, "user", "exec", "command"),
        operation: Op::Replace(Value::String(absolute.clone())),
    };

    let patched = apply_yaml_patches(&document, &[patch])?;

    tokio::fs::write(&kubeconfig_path, patched.source()).await?;

    progress.success(Some(&format!(
        "Replaced user {current_ctx_name:?}'s exec {cmd_path:?} with {absolute:?}"
    )));

    Ok(())
}
