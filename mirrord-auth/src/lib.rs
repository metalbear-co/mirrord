#[cfg(feature = "webbrowser")]
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
};

use lazy_static::lazy_static;
#[cfg(feature = "webbrowser")]
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};
use thiserror::Error;

lazy_static! {
    static ref FILE_DIR: PathBuf = [
        std::env::var("HOME")
            .or_else(|_| std::env::var("HOMEPATH"))
            .unwrap_or_else(|_| "~".to_owned()),
        ".mirrord_credentials".to_owned()
    ]
    .iter()
    .collect();
}

#[derive(Deserialize, Serialize)]
pub struct AuthConfig {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Error, Debug)]
pub enum AuthenticationError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    ConfigParseError(#[from] serde_json::Error),
    #[error(transparent)]
    #[cfg(feature = "webbrowser")]
    ConfigRequestError(#[from] reqwest::Error),
}

type Result<T> = std::result::Result<T, AuthenticationError>;

impl AuthConfig {
    pub fn config_path() -> &'static Path {
        FILE_DIR.as_path()
    }

    pub fn load() -> Result<AuthConfig> {
        let bytes = fs::read(FILE_DIR.as_path())?;

        serde_json::from_slice(&bytes).map_err(|err| err.into())
    }

    pub fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;

        fs::write(FILE_DIR.as_path(), bytes)?;

        Ok(())
    }

    pub fn from_input(token: &str) -> Result<Self> {
        let mut parts = token.split(':');

        let access_token = parts
            .next()
            .map(|val| val.to_owned())
            .expect("Invalid Token");
        let refresh_token = parts
            .next()
            .map(|val| val.to_owned())
            .expect("Invalid Token");

        Ok(AuthConfig {
            access_token,
            refresh_token,
        })
    }

    #[cfg(feature = "webbrowser")]
    pub fn from_webbrowser(server: &str, timeout: u64) -> Result<AuthConfig> {
        let ref_id = Alphanumeric.sample_string(&mut rand::thread_rng(), 16);

        let url = format!("{}/oauth?ref={}", server, ref_id);

        if let Err(_) = webbrowser::open(&url) {
            println!(
                "Problem auto launching webbrowser so please enter the following url in your webbrowser of choice\n\n url: {:?}\n",
                url
            );
        }

        let client = reqwest::blocking::Client::new();

        client
            .get(format!("{}/wait?ref={}", server, ref_id))
            .timeout(Duration::from_secs(timeout))
            .send()?
            .json()
            .map_err(|err| err.into())
    }
}
