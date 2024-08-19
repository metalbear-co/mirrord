use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use mirrord_protocol::{
    file::*, vpn::NetworkConfiguration, ClientMessage, DaemonMessage, FileRequest, FileResponse,
};
use tokio::process::Command;

use crate::{agent::VpnAgent, config::VpnConfig, error::VpnError};

#[derive(Debug)]
pub struct ResolvOverride {
    path: PathBuf,
    original_file: PathBuf,
}

impl ResolvOverride {
    pub async fn accuire_override<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = PathBuf::from(path.as_ref());
        let original_file = {
            let mut path = path.clone();

            path.set_file_name(match path.file_name().and_then(OsStr::to_str) {
                Some(filename) => format!("{filename}.backup"),
                None => "resolv.conf.backup".to_string(),
            });

            path
        };

        tokio::fs::copy(&path, &original_file).await?;

        Ok(ResolvOverride {
            path,
            original_file,
        })
    }

    pub async fn update_resolv(&self, bytes: &[u8]) -> io::Result<()> {
        tokio::fs::write(&self.path, bytes).await?;

        Ok(())
    }

    fn unmount(&self) -> io::Result<()> {
        let ResolvOverride {
            path,
            original_file,
        } = self;

        std::fs::copy(original_file, path)?;
        std::fs::remove_file(original_file)?;

        Ok(())
    }
}

impl Drop for ResolvOverride {
    fn drop(&mut self) {
        if let Err(error) = self.unmount() {
            tracing::warn!(%error, resolv_override = ?self, "unable to remove ResolvOverride")
        };
    }
}

fn get_file_response(message: DaemonMessage) -> Option<FileResponse> {
    match message {
        DaemonMessage::File(response) => Some(response),
        _ => None,
    }
}

async fn agent_fetch_file<const B: u64>(
    agent: &mut VpnAgent,
    path: PathBuf,
) -> Result<Vec<u8>, VpnError> {
    let request = FileRequest::Open(OpenFileRequest {
        path,
        open_options: OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    });

    let Some(FileResponse::Open(response)) = agent
        .send_and_get_response(ClientMessage::FileRequest(request), get_file_response)
        .await?
    else {
        return Err(VpnError::AgentUnexpcetedResponse);
    };

    let OpenFileResponse { fd } = response?;

    let request = FileRequest::Read(ReadFileRequest {
        remote_fd: fd,
        buffer_size: B,
    });

    let Some(FileResponse::Read(response)) = agent
        .send_and_get_response(ClientMessage::FileRequest(request), get_file_response)
        .await?
    else {
        return Err(VpnError::AgentUnexpcetedResponse);
    };

    let ReadFileResponse { bytes, .. } = response?;

    let request = FileRequest::Close(CloseFileRequest { fd });
    agent.send(ClientMessage::FileRequest(request)).await?;

    Ok(bytes)
}

pub async fn mount_linux<'a>(
    vpn_config: &'a VpnConfig,
    network: &'a NetworkConfiguration,
    vpn_agnet: &mut VpnAgent,
) -> Result<ResolvOverride, VpnError> {
    let remote_resolv = agent_fetch_file::<10000>(vpn_agnet, "/etc/resolv.conf".into()).await?;

    let resolv_override = ResolvOverride::accuire_override("/etc/resolv.conf")
        .await
        .map_err(VpnError::SetupIO)?;

    resolv_override
        .update_resolv(&remote_resolv)
        .await
        .map_err(VpnError::SetupIO)?;

    Command::new("ip")
        .args([
            "route".to_owned(),
            "add".to_owned(),
            vpn_config.service_subnet.to_string(),
            "via".to_owned(),
            network.gateway.to_string(),
        ])
        .output()
        .await
        .inspect_err(|error| tracing::error!(%error, "could not bind service_subnet"))
        .map_err(VpnError::SetupIO)?;

    Ok(resolv_override)
}
