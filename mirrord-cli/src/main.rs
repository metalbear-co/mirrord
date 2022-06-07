use std::{env::temp_dir, fs::File, io::Write, time::Duration};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use exec::execvp;
use semver::Version;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(subcommand)]
    commands: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Exec(ExecArgs),
    Extract { path: String },
}

#[derive(Args, Debug)]
struct ExecArgs {
    /// Pod name to mirror.
    #[clap(short, long)]
    pub pod_name: String,

    /// Namespace of the pod to mirror. Defaults to "default".
    #[clap(short = 'n', long)]
    pub pod_namespace: Option<String>,

    /// Namespace to place agent in.
    #[clap(short = 'a', long)]
    pub agent_namespace: Option<String>,

    /// Agent log level
    #[clap(short = 'l', long)]
    pub agent_log_level: Option<String>,

    /// Agent image
    #[clap(short = 'i', long)]
    pub agent_image: Option<String>,

    /// Enable file hooking
    #[clap(short = 'f', long)]
    pub enable_fs: bool,

    /// Binary to execute and mirror traffic into.
    #[clap()]
    pub binary: String,

    /// Accept/reject invalid certificates.
    #[clap(short = 'c', long)]
    pub accept_invalid_certificates: bool,
    /// Arguments to pass to the binary.
    #[clap()]
    binary_args: Vec<String>,
}

#[cfg(target_os = "linux")]
const INJECTION_ENV_VAR: &str = "LD_PRELOAD";

#[cfg(target_os = "macos")]
const INJECTION_ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

/// This is for Apple M1 running mirrord aarch trying to execute x86 binaries.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod mac_aarch {
    use std::{
        io::{Cursor, Read},
        path::Path,
    };

    use mach_object::{OFile, CPU_TYPE_X86_64};
    use reqwest;
    use search_path::SearchPath;

    use super::*;
    pub fn is_binary_different_arch(binary_path: &String) -> bool {
        let search_path = SearchPath::new("PATH").unwrap();
        let binary_path = match search_path.find(&Path::new(binary_path)) {
            Some(path) => path,
            None => {
                warn!("Could not find binary in PATH");
                return false;
            }
        };
        let mut binary_file = match File::open(binary_path) {
            Ok(file) => file,
            Err(err) => {
                warn!("Could not open binary: {err:?}");
                return false;
            }
        };
        let mut binary_content = Vec::new();
        let size = match binary_file.read_to_end(&mut binary_content) {
            Ok(size) => size,
            Err(err) => {
                warn!("Could not read binary: {err:?}");
                return false;
            }
        };
        let mut binary_cursor = Cursor::new(&binary_content[..size]);
        match OFile::parse(&mut binary_cursor) {
            Ok(OFile::MachFile {
                header,
                commands: _,
            }) => header.cputype == CPU_TYPE_X86_64,
            Ok(_) => {
                warn!("Could not parse binary as Mach-O File");
                false
            }
            Err(err) => {
                warn!("Could not open binary: {err:?}");
                false
            }
        }
    }

    pub fn extract_intel_binary() -> Result<String> {
        let file_name = format!("libmirrord_layer_intel_{}.so", env!("CARGO_PKG_VERSION"));
        let file_path = temp_dir().as_path().join(file_name);
        // Don't download file if not needed
        if file_path.exists() {
            info!("x86 mode, found existing dylib to use");
            return Ok(file_path.to_str().unwrap().to_string());
        }
        info!("download x86 dylib for rosetta mode");
        let download_url = format!("https://github.com/metalbear-co/mirrord/releases/download/{}/libmirrord_layer_mac_x86_64.dylib", env!("CARGO_PKG_VERSION"));
        let mut response =
            reqwest::blocking::get(download_url).context("Could not download intel binary")?;
        let mut file = File::create(&file_path).context("Could not create intel binary")?;
        response
            .copy_to(&mut file)
            .context("Could not write intel binary")?;
        Ok(file_path.to_str().unwrap().to_string())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use mac_aarch::*;

fn extract_library(dest_dir: Option<String>) -> Result<String> {
    let library_file = env!("CARGO_CDYLIB_FILE_MIRRORD_LAYER");
    let library_path = std::path::Path::new(library_file);

    let file_name = library_path.components().last().unwrap();
    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => temp_dir().as_path().join(file_name),
    };
    let mut file = File::create(&file_path)
        .with_context(|| format!("Path \"{}\" creation failed", file_path.display()))?;
    let bytes = include_bytes!(env!("CARGO_CDYLIB_FILE_MIRRORD_LAYER"));
    file.write_all(bytes).unwrap();

    debug!("Extracted library file to {:?}", &file_path);
    Ok(file_path.to_str().unwrap().to_string())
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

    if let Some(log_level) = &args.agent_log_level {
        std::env::set_var("MIRRORD_AGENT_RUST_LOG", log_level.clone());
    }

    if let Some(image) = &args.agent_image {
        std::env::set_var("MIRRORD_AGENT_IMAGE", image.clone());
    }

    if args.enable_fs {
        std::env::set_var("MIRRORD_FILE_OPS", true.to_string());
    }

    if args.accept_invalid_certificates {
        std::env::set_var("MIRRORD_ACCEPT_INVALID_CERTIFICATES", "true");
    }
    let library_path = {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            if is_binary_different_arch(&args.binary) {
                extract_intel_binary()?
            } else {
                extract_library(None)?
            }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            extract_library(None)?
        }
    };
    add_to_preload(&library_path).unwrap();

    let mut binary_args = args.binary_args.clone();
    binary_args.insert(0, args.binary.clone());

    let err = execvp(args.binary.clone(), binary_args);
    error!("Couldn't execute {:?}", err);
    Err(anyhow!("Failed to execute binary"))
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
