use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write, stdin, stdout},
    path::PathBuf,
};

use kube::config::{Kubeconfig, KubeconfigError};
use serde_yaml::Value;
use yamlpatch::{Op, Patch, apply_yaml_patches};
use yamlpath::{Document, route};

use crate::{
    CliResult,
    config::{FixArgs, FixCommand, FixKubeconfig},
};

pub async fn fix_command(args: FixArgs) -> CliResult<()> {
    match args.command {
        FixCommand::Kubeconfig(args) => fix_kubeconfig(args).await?,
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
}

async fn fix_kubeconfig(args: FixKubeconfig) -> Result<(), FixKubeconfigError> {
    let kubeconfig_path = args.file_path.unwrap_or(
        std::env::home_dir()
            .ok_or(FixKubeconfigError::Home)?
            .join(".kube/config"),
    );
    let config = Kubeconfig::read_from(&kubeconfig_path)?;

    for (user_idx, user) in config.auth_infos.iter().enumerate() {
        let Some(auth) = &user.auth_info else {
            continue;
        };

        let Some(exec) = &auth.exec else {
            continue;
        };

        let Some(command) = &exec.command else {
            continue;
        };

        let cmd_path = PathBuf::from(command);

        if cmd_path.is_absolute() {
            continue;
        }

        let absolute = match which::which(&cmd_path) {
            Ok(path) => path
                .into_os_string()
                .into_string()
                .map_err(FixKubeconfigError::StringConversion)?,
            Err(err) => {
                println!(
                    "WARNING: user `{}` has an auth config that uses `exec` with non-absolute binary {cmd_path:?}, which cannot be resolved using the current $PATH ({err})",
                    user.name
                );
                continue;
            }
        };

        let replace = loop {
            print!(
                "User `{}` has an auth config that uses `exec` with non-absolute binary {cmd_path:?}, located at {absolute:?}. Replace with absolute path? [y/n] ",
                user.name
            );
            stdout().flush()?;

            let mut answer = String::new();
            let mut reader = BufReader::new(stdin());
            reader.read_line(&mut answer)?;

            match &*answer {
                "y\n" => {
                    break true;
                }
                "n\n" => {
                    break false;
                }
                _ => {}
            }
        };

        if !replace {
            continue;
        }

        let document = Document::new(tokio::fs::read_to_string(&kubeconfig_path).await?)?;

        let patch = Patch {
            route: route!("users", user_idx, "user", "exec", "command"),
            operation: Op::Replace(Value::String(absolute)),
        };

        let patched = apply_yaml_patches(&document, &[patch])?;

        tokio::fs::write(&kubeconfig_path, patched.source()).await?;
    }

    Ok(())
}
