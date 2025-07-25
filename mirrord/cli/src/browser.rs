#![cfg(not(windows))]
// Currently browser only supported on not(windows)

use std::{process::Command, sync::LazyLock};

use base64::engine::{general_purpose::STANDARD, Engine};
use mirrord_config::feature::network::NetworkConfig;
use mirrord_progress::Progress;
use serde::Serialize;

// Permanent ID once extension is published
static MIRRORD_CHROME_EXTENSION_ID: LazyLock<&str> = LazyLock::new(|| {
    option_env!("MIRRORD_CHROME_EXTENSION_ID").unwrap_or("bijejadnnfgjkfdocgocklekjhnhkhkf")
});

pub(crate) fn init_browser_extension<P>(network_config: &NetworkConfig, progress: &P)
where
    P: Progress,
{
    let mut sub_progress_browser = progress.subtask("browser extension config");
    let Some(init_url) = extension_init_url(network_config) else {
        sub_progress_browser.warning("network config unsupported by browser extension");
        return;
    };
    sub_progress_browser.info(&format!("browser extension config URL: {init_url}"));
    apply_extension_config(&init_url, &mut sub_progress_browser);
}

fn extension_init_url(config: &NetworkConfig) -> Option<String> {
    let payload = ExtensionInitPayload {
        header_filter: config.incoming.http_filter.header_filter.clone()?,
    };
    Some(format!(
        "chrome-extension://{extension_id}/pages/config.html?payload={payload}",
        extension_id = *MIRRORD_CHROME_EXTENSION_ID,
        payload = payload.encode()?,
    ))
}

fn apply_extension_config<P>(init_url: &str, progress: &mut P)
where
    P: Progress,
{
    fn open_in_chrome(url: &str) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            Command::new("open")
                .args(["-a", "Google Chrome", url])
                .status()
                .map(|_| ())
        }

        #[cfg(target_os = "linux")]
        {
            Command::new("google-chrome").arg(url).status().map(|_| ())
        }
    }
    if is_chrome_installed() {
        if open_in_chrome(init_url).is_ok() {
            progress.success(Some("browser extension configured"));
        } else {
            progress.failure(None);
        }
    } else {
        progress.failure(Some("cannot find Google Chrome"));
    }
}

#[cfg(target_os = "macos")]
fn is_chrome_installed() -> bool {
    Command::new("open")
        .args(["-Ra", "Google Chrome"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_chrome_installed() -> bool {
    Command::new("which")
        .arg("google-chrome")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_chrome_installed() -> bool {
    Command::new("cmd")
        .args(&["/c", "where", "chrome"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[derive(Serialize)]
struct ExtensionInitPayload {
    header_filter: String,
}

impl ExtensionInitPayload {
    fn encode(&self) -> Option<String> {
        Some(STANDARD.encode(serde_json::to_string(self).ok()?))
    }
}
