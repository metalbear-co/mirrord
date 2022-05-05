use envconfig::Envconfig;
use lazy_static::lazy_static;

#[derive(Envconfig)]
pub struct Config {
    #[envconfig(from = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub agent_rust_log: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub impersonated_pod_name: String,
}

lazy_static! {
    pub static ref CONFIG: Config = Config::init_from_env().unwrap();
}
