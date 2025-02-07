use std::{collections::HashMap, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

pub type StealTlsConfig = HashMap<u16, StealPortTlsConfig>;

#[derive(Deserialize, Serialize)]
pub struct StealPortTlsConfig {
    pub server_cert_pem: PathBuf,
    pub server_key_pem: PathBuf,
    pub alpn_protocols: Vec<AlpnProtocol>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum AlpnProtocol {
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "http/1.0")]
    Http10,
    #[serde(rename = "http/1.1")]
    Http11,
}

impl fmt::Display for AlpnProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::H2 => "h2",
            Self::Http10 => "http/1.0",
            Self::Http11 => "http/1.1",
        };

        f.write_str(as_str)
    }
}
