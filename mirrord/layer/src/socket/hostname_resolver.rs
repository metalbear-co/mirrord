//! Unix-specific hostname resolver implementation

use std::{ffi::CString, path::PathBuf};

use mirrord_layer_lib::{HostnameResolver, HostnameResult, HostnameError};
use mirrord_protocol::file::{OpenFileResponse, OpenOptionsInternal, ReadFileResponse};
use tracing::trace;

use crate::{detour::{Detour, Bypass}, file};

/// Unix-specific hostname resolver that fetches hostname from remote /etc/hostname
pub struct UnixHostnameResolver;

impl HostnameResolver for UnixHostnameResolver {
    fn fetch_remote_hostname(&self) -> HostnameResult {
        match self.fetch_remote_hostname_detour() {
            Detour::Success(hostname) => HostnameResult::Success(hostname),
            Detour::Bypass(Bypass::LocalHostname) => HostnameResult::UseLocal,
            Detour::Bypass(_) => HostnameResult::Error(HostnameError::Protocol("Bypass encountered".to_string())),
            Detour::Error(e) => HostnameResult::Error(HostnameError::Protocol(format!("Hook error: {e:?}"))),
        }
    }
    
    fn should_use_local_hostname(&self) -> bool {
        crate::setup().local_hostname()
    }
}

impl UnixHostnameResolver {
    /// Internal implementation that returns Detour for compatibility with existing code
    pub fn fetch_remote_hostname_detour(&self) -> Detour<CString> {
        remote_hostname_string()
    }
}

/// Retrieves the `hostname` from the agent's `/etc/hostname` to be used by [`gethostname`]
fn remote_hostname_string() -> Detour<CString> {
    if crate::setup().local_hostname() {
        Detour::Bypass(Bypass::LocalHostname)?;
    }

    let hostname_path = PathBuf::from("/etc/hostname");

    let OpenFileResponse { fd } = file::ops::RemoteFile::remote_open(
        hostname_path,
        OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    )?;

    let ReadFileResponse { bytes, read_amount } = file::ops::RemoteFile::remote_read(fd, 256)?;

    let _ = file::ops::RemoteFile::remote_close(fd).inspect_err(|fail| {
        trace!("Leaking remote file fd (should be harmless) due to {fail:#?}!")
    });

    CString::new(
        bytes
            .into_vec()
            .into_iter()
            .take(read_amount as usize - 1)
            .collect::<Vec<_>>(),
    )
    .map(Detour::Success)?
}
