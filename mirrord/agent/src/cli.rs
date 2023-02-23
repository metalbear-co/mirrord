#![deny(missing_docs)]

use clap::{
    error::{Error, ErrorKind},
    Parser,
};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Container id to get traffic from
    #[arg(short, long)]
    pub container_id: Option<String>,

    /// Container runtime to use
    #[arg(short = 'r', long)]
    pub container_runtime: Option<String>,

    /// Port to use for communication
    #[arg(short = 'l', long, default_value_t = 61337)]
    pub communicate_port: u16,

    /// Communication timeout in seconds
    #[arg(short = 't', long, default_value_t = 30)]
    pub communication_timeout: u16,

    /// Interface to use
    #[arg(short = 'i', long)]
    pub network_interface: Option<String>,

    /// Inform the agent to use `proc/1/root` as the root directory.
    #[arg(short = 'e', long, default_value_t = false)]
    pub ephemeral_container: bool,

    /// Pause the target container while clients are connected.
    #[arg(short = 'p', long, default_value_t = false)]
    pub pause: bool,

    /// Return an error after accepting the first client connection, in order to test agent error
    /// cleanup.
    #[arg(long, default_value_t = false, hide = true)]
    pub test_error: bool,
}

const DEFAULT_RUNTIME: &str = "containerd";

pub fn parse_args() -> Args {
    let args = Args::try_parse()
        .and_then(|args| {
            if args.container_runtime.is_some() && args.container_id.is_none() {
                Err(Error::raw(
                    ErrorKind::InvalidValue,
                    "container_id is required when container_runtime is specified",
                ))
            } else {
                Ok(args)
            }
        })
        .map(|mut args| {
            if args.container_runtime.is_none() {
                args.container_runtime = Some(DEFAULT_RUNTIME.to_string());
            }
            args
        });
    match args {
        Ok(args) => args,
        Err(e) => e.exit(),
    }
}
