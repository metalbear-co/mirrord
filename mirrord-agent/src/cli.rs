use clap::{
    error::{Error, ErrorKind},
    Parser,
};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    /// Container id to get traffic from
    #[clap(short, long, value_parser)]
    pub container_id: Option<String>,

    /// Container runtime to use
    #[clap(short = 'r', long, value_parser)]
    pub container_runtime: Option<String>,

    /// Port to use for communication
    #[clap(short = 'l', long, default_value_t = 61337, value_parser)]
    pub communicate_port: u16,

    /// Communication timeout in seconds
    #[clap(short = 't', long, default_value_t = 30, value_parser)]
    pub communication_timeout: u16,

    /// Interface to use
    #[clap(short = 'i', long, default_value = "eth0", value_parser)]
    pub interface: String,
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
