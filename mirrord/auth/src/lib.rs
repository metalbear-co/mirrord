#![warn(clippy::indexing_slicing)]

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
    static ref HOME_DIR: PathBuf = [
        std::env::var("HOME")
            .or_else(|_| std::env::var("HOMEPATH"))
            .unwrap_or_else(|_| "~".to_owned()),
        ".metalbear_credentials".to_owned()
    ]
    .iter()
    .collect();
    static ref AUTH_FILE_DIR: PathBuf = std::env::var("MIRRORD_AUTHENTICATION")
        .ok()
        .and_then(|val| val.parse().ok())
        .unwrap_or_else(|| HOME_DIR.to_path_buf());
}

#[derive(Deserialize, Serialize)]
pub struct AuthConfig {
    pub access_token: String,
    pub refresh_token: Option<String>,
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
        AUTH_FILE_DIR.as_path()
    }

    pub fn load() -> Result<AuthConfig> {
        let bytes = fs::read(AUTH_FILE_DIR.as_path())?;

        serde_json::from_slice(&bytes).map_err(|err| err.into())
    }

    pub fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;

        fs::write(AUTH_FILE_DIR.as_path(), bytes)?;

        Ok(())
    }

    pub fn from_input(token: &str) -> Result<Self> {
        let mut parts = token.split(':');

        let access_token = parts
            .next()
            .map(|val| val.to_owned())
            .expect("Invalid Token");
        let refresh_token = parts.next().map(|val| val.to_owned());

        Ok(AuthConfig {
            access_token,
            refresh_token,
        })
    }

    #[cfg(feature = "webbrowser")]
    pub fn from_webbrowser(server: &str, timeout: u64, no_open: bool) -> Result<AuthConfig> {
        let ref_id = Alphanumeric.sample_string(&mut rand::thread_rng(), 16);

        let url = format!("{server}/oauth?ref={ref_id}");

        let url_print = format!(
            "Please enter the following url in your webbrowser of choice\n\n url: {url:?}\n"
        );

        if no_open {
            println!("{url_print}");
        } else if webbrowser::open(&url).is_err() {
            println!("Problem auto launching webbrowser\n{url_print}");
        }

        let client = reqwest::blocking::Client::new();

        client
            .get(format!("{server}/wait?ref={ref_id}"))
            .timeout(Duration::from_secs(timeout))
            .send()?
            .json()
            .map_err(|err| err.into())
    }
}
