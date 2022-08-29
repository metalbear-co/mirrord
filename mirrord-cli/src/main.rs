use std::{fs::File, io::Write, path::PathBuf, time::Duration};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use config::*;
use exec::execvp;
use mirrord_auth::AuthConfig;
use semver::Version;
use tracing::{debug, error, info};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

mod config;

#[cfg(target_os = "linux")]
const INJECTION_ENV_VAR: &str = "LD_PRELOAD";

#[cfg(target_os = "macos")]
const INJECTION_ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

/// For some reason loading dylib from $TMPDIR can get the process killed somehow..?
#[cfg(target_os = "macos")]
mod mac {
    use std::str::FromStr;

    use super::*;

    pub fn temp_dir() -> PathBuf {
        PathBuf::from_str("/tmp/").unwrap()
    }
}

#[cfg(not(target_os = "macos"))]
use std::env::temp_dir;

#[cfg(target_os = "macos")]
use mac::temp_dir;

fn extract_library(dest_dir: Option<String>) -> Result<PathBuf> {
    let library_file = env!("MIRRORD_LAYER_FILE");
    let library_path = std::path::Path::new(library_file);

    let file_name = library_path.components().last().unwrap();
    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => temp_dir().as_path().join(file_name),
    };
    let mut file = File::create(&file_path)
        .with_context(|| format!("Path \"{}\" creation failed", file_path.display()))?;
    let bytes = include_bytes!(env!("MIRRORD_LAYER_FILE"));
    file.write_all(bytes).unwrap();

    debug!("Extracted library file to {:?}", &file_path);
    Ok(file_path)
}

fn add_to_preload(path: &str) -> Result<()> {
    match std::env::var(INJECTION_ENV_VAR) {
        Ok(value) => {
            let new_value = format!("{}:{}", value, path);
            std::env::set_var(INJECTION_ENV_VAR, new_value);
            Ok(())
        }
        Err(std::env::VarError::NotPresent) => {
            std::env::set_var(INJECTION_ENV_VAR, path);
            Ok(())
        }
        Err(e) => {
            error!("Failed to set environment variable with error {:?}", e);
            Err(anyhow!("Failed to set environment variable"))
        }
    }
}

fn exec(args: &ExecArgs) -> Result<()> {
    info!(
        "Launching {:?} with arguments {:?}",
        args.binary, args.binary_args
    );
    std::env::set_var("MIRRORD_AGENT_IMPERSONATED_POD_NAME", args.pod_name.clone());

    if let Some(namespace) = &args.pod_namespace {
        std::env::set_var(
            "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE",
            namespace.clone(),
        );
    }

    if let Some(namespace) = &args.agent_namespace {
        std::env::set_var("MIRRORD_AGENT_NAMESPACE", namespace.clone());
    }

    if let Some(impersonated_container_name) = &args.impersonated_container_name {
        std::env::set_var(
            "MIRRORD_IMPERSONATED_CONTAINER_NAME",
            impersonated_container_name,
        );
    }

    if let Some(log_level) = &args.agent_log_level {
        std::env::set_var("MIRRORD_AGENT_RUST_LOG", log_level.clone());
    }

    if let Some(image) = &args.agent_image {
        std::env::set_var("MIRRORD_AGENT_IMAGE", image.clone());
    }

    if let Some(agent_ttl) = &args.agent_ttl {
        std::env::set_var("MIRRORD_AGENT_TTL", agent_ttl.to_string());
    }

    if args.enable_fs {
        std::env::set_var("MIRRORD_FILE_OPS", true.to_string());
    }

    if let Some(override_env_vars_exclude) = &args.override_env_vars_exclude {
        std::env::set_var(
            "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE",
            override_env_vars_exclude,
        );
    }

    if let Some(override_env_vars_include) = &args.override_env_vars_include {
        std::env::set_var(
            "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE",
            override_env_vars_include,
        );
    }

    if args.remote_dns {
        std::env::set_var("MIRRORD_REMOTE_DNS", true.to_string());
    }

    if args.accept_invalid_certificates {
        std::env::set_var("MIRRORD_ACCEPT_INVALID_CERTIFICATES", "true");
    }

    if args.ephemeral_container {
        std::env::set_var("MIRRORD_EPHEMERAL_CONTAINER", "true");
    };

    if args.enable_tcp_outgoing {
        std::env::set_var("MIRRORD_TCP_OUTGOING", true.to_string());
    }

    let library_path = extract_library(args.extract_path.clone())?;
    add_to_preload(library_path.to_str().unwrap()).unwrap();

    let mut binary_args = args.binary_args.clone();
    binary_args.insert(0, args.binary.clone());

    let err = execvp(args.binary.clone(), binary_args);
    error!("Couldn't execute {:?}", err);
    Err(anyhow!("Failed to execute binary"))
}

fn login(args: LoginArgs) -> Result<()> {
    match &args.token {
        Some(token) => AuthConfig::from_input(token)?.save()?,
        None => {
            AuthConfig::from_webbrowser(&args.auth_server, args.timeout, args.no_open)?.save()?
        }
    }

    println!(
        "Config succesfuly saved at {}",
        AuthConfig::config_path().display()
    );

    Ok(())
}

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
fn main() -> Result<()> {
    registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
    prompt_outdated_version();

    let cli = Cli::parse();
    match cli.commands {
        Commands::Exec(args) => exec(&args)?,
        Commands::Extract { path } => {
            extract_library(Some(path))?;
        }
        Commands::Login(args) => login(args)?,
    }
    Ok(())
}

fn prompt_outdated_version() {
    let check_version: bool = std::env::var("MIRRORD_CHECK_VERSION")
        .map(|s| s.parse().unwrap_or(true))
        .unwrap_or(true);

    if check_version {
        if let Ok(client) = reqwest::blocking::Client::builder().build() {
            if let Ok(result) = client
                .get(format!(
                    "https://version.mirrord.dev/get-latest-version?source=2&currentVersion={}",
                    CURRENT_VERSION
                ))
                .timeout(Duration::from_secs(1))
                .send()
            {
                if let Ok(latest_version) = Version::parse(&result.text().unwrap()) {
                    if latest_version > Version::parse(CURRENT_VERSION).unwrap() {
                        println!("New mirrord version available: {}. To update, run: `curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash`.", latest_version);
                        println!("To disable version checks, set env variable MIRRORD_CHECK_VERSION to 'false'.")
                    }
                }
            }
        }
    }
}
