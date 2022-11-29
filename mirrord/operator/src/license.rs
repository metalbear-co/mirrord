use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "license-fetch")]
static LICENSE_SERVER: &str = "https://license.metalbear.co/v1/check";

#[derive(Serialize, Deserialize, Debug)]
pub struct License {
    pub organization: String,
    pub name: String,
    pub expire_at: NaiveDate,
}

impl License {
    pub fn is_expired(&self) -> bool {
        self.expire_at <= Utc::now().date_naive()
    }
}

#[cfg(feature = "license-fetch")]
impl License {
    pub fn fetch(api_key: String) -> Result<Self, reqwest::Error> {
        let request = LicenseCheckRequest { api_key };

        reqwest::blocking::Client::new()
            .post(LICENSE_SERVER)
            .json(&request)
            .send()?
            .json()
            .map(License::from)
    }

    pub async fn fetch_async(api_key: String) -> Result<Self, reqwest::Error> {
        let request = LicenseCheckRequest { api_key };

        reqwest::Client::new()
            .post(LICENSE_SERVER)
            .json(&request)
            .send()
            .await?
            .json()
            .await
            .map(License::from)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LicenseCheckRequest {
    api_key: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LicenseCheckResponse(License);

impl Into<License> for LicenseCheckResponse {
    fn into(self) -> License {
        self.0
    }
}
