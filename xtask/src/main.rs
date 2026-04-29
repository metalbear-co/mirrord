mod tasks;

use std::process::Command;

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
        /// Target platform (linux-x86_64, linux-aarch64, macos-x86_64, macos-aarch64,
        /// macos-universal, windows)
        #[arg(short, long, value_parser = parse_platform)]
        platform: Option<Platform>,

        /// Build in release mode
        #[arg(short, long)]
        release: bool,

        /// Build without wizard frontend
        #[arg(long)]
        no_wizard: bool,

        /// Build without monitor frontend
        #[arg(long)]
        no_monitor: bool,

        /// Use the existing wizard dist without rebuilding frontend assets
        #[arg(long, conflicts_with = "no_wizard")]
        skip_build_wizard: bool,

        /// Additional arguments passed to cargo
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Build wizard frontend only
    BuildWizard,

    /// Prepare monitor frontend assets only
    BuildMonitor,

    /// Build layer only
    BuildLayer {
        /// Target platform
        #[arg(short, long, value_parser = parse_platform)]
        platform: Option<Platform>,

        /// Build in release mode
        #[arg(short, long)]
        release: bool,

        /// Additional arguments passed to cargo
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Link pre-built architecture-specific layers into universal binary
    LinkLayerUniversal {
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
    },

    /// Merge pre-built architecture-specific CLIs into universal binary
    MergeCliUniversal {
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
    },

    /// Run `cargo doc --document-private-items --no-deps` with dummy layer artifacts
    TestDoc {
        /// Additional arguments passed to `cargo doc`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Run the e2e test suite, optionally with externally provided mirrord artifacts
    TestE2e {
        /// Path to an external mirrord CLI binary
        #[arg(long)]
        binary: Option<std::path::PathBuf>,

        /// Path to an external mirrord layer file
        #[arg(long)]
        layer: Option<std::path::PathBuf>,

        /// Additional arguments passed to `cargo test` or `cargo nextest run`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Run unit tests for the mirrord CLI with placeholder embedded assets
    TestUt {
        /// Additional arguments passed to `cargo test`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Run the integration test suite, optionally with externally provided mirrord artifacts
    TestIntegration {
        /// Path to an external mirrord CLI binary
        #[arg(long)]
        binary: Option<std::path::PathBuf>,

        /// Path to an external mirrord layer file
        #[arg(long)]
        layer: Option<std::path::PathBuf>,

        /// Additional arguments passed to `cargo test` or `cargo nextest run`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Build and run the CLI, analogous to `cargo run`. Does not
    /// build the wizard and monitor by default.
    RunCli {
        /// Build with wizard frontend
        #[arg(long)]
        build_wizard: bool,

        /// Build with monitor frontend
        #[arg(long)]
        build_monitor: bool,

        /// CLI args for mirrord
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        mirrord_args: Vec<String>,
    },
}

fn parse_platform(s: &str) -> Result<Platform, String> {
    match s {
        "linux-x86_64" | "linux-x86-64" | "linux-amd64" => Ok(Platform::LinuxX86_64),
        "linux-aarch64" | "linux-arm64" => Ok(Platform::LinuxAarch64),
        "macos-x86_64" | "macos-x86-64" | "macos-intel" => Ok(Platform::MacosX86_64),
        "macos-aarch64" | "macos-arm64" | "macos-apple-silicon" => Ok(Platform::MacosAarch64),
        "macos-universal" | "macos" | "darwin" => Ok(Platform::MacosUniversal),
        "windows" | "win" => Ok(Platform::Windows),
        _ => Err(format!(
            "Invalid platform: {}. Valid options: linux-x86_64, linux-aarch64, macos-x86_64, macos-aarch64, macos-universal, windows",
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
            no_monitor,
            skip_build_wizard,
            cargo_args,
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
                build_monitor: !no_monitor,
                with_wizard: !no_wizard,
                build_wizard: !no_wizard && !skip_build_wizard,
                cargo_args,
                quiet: false,
            };

            tasks::release::build_release_cli(options)?;
        }

        Commands::BuildWizard => {
            tasks::wizard::build_wizard()?;
        }

        Commands::BuildMonitor => {
            tasks::monitor::build_monitor()?;
        }

        Commands::BuildLayer {
            platform,
            release,
            cargo_args,
        } => {
            let platform = platform.unwrap_or_else(|| {
                Platform::detect().unwrap_or_else(|e| {
                    eprintln!("Failed to detect platform: {}", e);
                    eprintln!("Please specify platform with --platform");
                    std::process::exit(1);
                })
            });

            match platform {
                Platform::MacosX86_64 => {
                    tasks::layer::build_layer(
                        tasks::layer::Target::MacosX86_64,
                        release,
                        &cargo_args,
                    )?;
                }
                Platform::MacosAarch64 => {
                    tasks::layer::build_layer(
                        tasks::layer::Target::MacosAarch64,
                        release,
                        &cargo_args,
                    )?;
                    tasks::layer::build_shim(release)?;
                }
                Platform::MacosUniversal => {
                    tasks::layer::build_macos_universal_layer(release, &cargo_args)?;
                }
                Platform::LinuxX86_64 => {
                    tasks::layer::build_layer(
                        tasks::layer::Target::LinuxX86_64,
                        release,
                        &cargo_args,
                    )?;
                }
                Platform::LinuxAarch64 => {
                    tasks::layer::build_layer(
                        tasks::layer::Target::LinuxAarch64,
                        release,
                        &cargo_args,
                    )?;
                }
                Platform::Windows => {
                    tasks::layer::build_layer(tasks::layer::Target::Windows, release, &cargo_args)?;
                }
            }
        }

        Commands::LinkLayerUniversal { release } => {
            tasks::layer::link_macos_universal_layer(release)?;
        }

        Commands::MergeCliUniversal { release } => {
            tasks::cli::merge_macos_universal_cli(release)?;
        }

        Commands::TestDoc { cargo_args } => {
            tasks::doc::run(cargo_args)?;
        }

        Commands::TestE2e {
            binary,
            layer,
            cargo_args,
        } => {
            tasks::test::run(tasks::test::Suite::E2e, binary, layer, cargo_args)?;
        }

        Commands::TestUt { cargo_args } => {
            tasks::test::run_unit(cargo_args)?;
        }

        Commands::TestIntegration {
            binary,
            layer,
            cargo_args,
        } => {
            tasks::test::run(tasks::test::Suite::Integration, binary, layer, cargo_args)?;
        }

        Commands::RunCli {
            build_wizard,
            build_monitor,
            mirrord_args,
        } => {
            let platform = Platform::detect().unwrap_or_else(|e| {
                eprintln!("Failed to detect platform: {}", e);
                eprintln!("Please specify platform with --platform");
                std::process::exit(1);
            });

            let options = BuildOptions {
                platform,
                release: false,
                build_monitor,
                with_wizard: build_wizard,
                build_wizard,
                cargo_args: vec![], // welp
                quiet: true,
            };

            let cli_path = tasks::release::build_release_cli(options)?;

            match Command::new(cli_path).args(mirrord_args).spawn() {
                Ok(mut child) => {
                    if let Err(err) = child.wait() {
                        println!("error while waiting for child: {err}");
                    };
                }
                Err(err) => {
                    println!("failed to run child process: {err}");
                }
            }
        }
    }

    Ok(())
}
