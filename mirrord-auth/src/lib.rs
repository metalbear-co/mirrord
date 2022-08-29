#[cfg(feature = "webbrowser")]
use std::time::Duration;

use lazy_static::lazy_static;
#[cfg(feature = "webbrowser")]
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};
use thiserror::Error;

lazy_static! {
    static ref KEYRING_SERVICE: String =
        std::env::var("MIRRORD_KEYRING_SERVICE").unwrap_or_else(|_| "mirrord".to_owned());
}

#[derive(Deserialize, Serialize)]
pub struct AuthConfig {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Error, Debug)]
pub enum AuthenticationError {
    #[error(transparent)]
    Keyring(#[from] keyring::Error),
    #[error(transparent)]
    ConfigParse(#[from] serde_json::Error),
    #[error(transparent)]
    #[cfg(feature = "webbrowser")]
    ConfigRequest(#[from] reqwest::Error),
}

type Result<T> = std::result::Result<T, AuthenticationError>;

impl AuthConfig {
    pub fn load(username: Option<String>) -> Result<AuthConfig> {
        let username = username.unwrap_or_else(whoami::username);

        let entry = keyring::Entry::new(&KEYRING_SERVICE, &username);

        let password = entry.get_password()?;

        serde_json::from_str(&password).map_err(|err| err.into())
    }

    pub fn save(&self, username: Option<String>) -> Result<()> {
        let username = username.unwrap_or_else(whoami::username);

        let entry = keyring::Entry::new(&KEYRING_SERVICE, &username);

        let password = serde_json::to_string(self)?;

        entry.set_password(&password)?;

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

        let url = format!("{}/oauth?ref={}", server, ref_id);

        let url_print = format!(
            "Please enter the following url in your webbrowser of choice\n\n url: {:?}\n",
            url
        );

        if no_open {
            println!("{}", url_print);
        } else if webbrowser::open(&url).is_err() {
            println!("Problem auto launching webbrowser\n{}", url_print);
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
