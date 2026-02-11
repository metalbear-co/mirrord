mod tasks;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tasks::release::{BuildOptions, Platform};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Build automation for mirrord", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build CLI binaries (includes wizard and layer)
    BuildCli {
        /// Target platform (linux-x86_64, linux-aarch64, macos-universal, windows)
        #[arg(short, long, value_parser = parse_platform)]
        platform: Option<Platform>,

        /// Build in release mode
        #[arg(short, long)]
        release: bool,

        /// Build without wizard frontend
        #[arg(long)]
        no_wizard: bool,
    },

    /// Build wizard frontend only
    BuildWizard,

    /// Build layer only
    BuildLayer {
        /// Target platform
        #[arg(short, long, value_parser = parse_platform)]
        platform: Option<Platform>,

        /// Build in release mode
        #[arg(short, long)]
        release: bool,
    },
}

fn parse_platform(s: &str) -> Result<Platform, String> {
    match s {
        "linux-x86_64" | "linux-x86-64" | "linux-amd64" => Ok(Platform::LinuxX86_64),
        "linux-aarch64" | "linux-arm64" => Ok(Platform::LinuxAarch64),
        "macos-universal" | "macos" | "darwin" => Ok(Platform::MacosUniversal),
        "windows" | "win" => Ok(Platform::Windows),
        _ => Err(format!(
            "Invalid platform: {}. Valid options: linux-x86_64, linux-aarch64, macos-universal, windows",
            s
        )),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::BuildCli {
            platform,
            release,
            no_wizard,
        } => {
            let platform = platform.unwrap_or_else(|| {
                Platform::detect().unwrap_or_else(|e| {
                    eprintln!("Failed to detect platform: {}", e);
                    eprintln!("Please specify platform with --platform");
                    std::process::exit(1);
                })
            });

            let options = BuildOptions {
                platform,
                release,
                with_wizard: !no_wizard,
            };

            tasks::release::build_release_cli(options)?;
        }

        Commands::BuildWizard => {
            tasks::wizard::build_wizard()?;
        }

        Commands::BuildLayer { platform, release } => {
            let platform = platform.unwrap_or_else(|| {
                Platform::detect().unwrap_or_else(|e| {
                    eprintln!("Failed to detect platform: {}", e);
                    eprintln!("Please specify platform with --platform");
                    std::process::exit(1);
                })
            });

            match platform {
                Platform::MacosUniversal => {
                    tasks::layer::build_macos_universal_layer(release)?;
                }
                Platform::LinuxX86_64 => {
                    tasks::layer::build_layer(tasks::layer::Target::LinuxX86_64, release)?;
                }
                Platform::LinuxAarch64 => {
                    tasks::layer::build_layer(tasks::layer::Target::LinuxAarch64, release)?;
                }
                Platform::Windows => {
                    tasks::layer::build_layer(tasks::layer::Target::Windows, release)?;
                }
            }
        }
    }

    Ok(())
}
