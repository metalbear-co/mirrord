//! Prints what the user must know about the share link of a session started with `share`: the
//! link, the reason it failed, or why it isn't there.
//!
//! The internal proxy runs detached from the user's terminal, so it sends these reports to the
//! application over a share control connection, and a thread in the application prints them. The
//! connection belongs to this process: the internal proxy treats its EOF as this process exiting,
//! even when a forked child still holds a copy of the request connection.
//!
//! In `json` mode the reports go to stdout, where a program that reads mirrord's progress, such as
//! an IDE, also reads `Ready!`. Otherwise they go to stderr, with the application's own messages.

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{
    net::{Shutdown, SocketAddr, TcpStream},
    ops::Not,
    sync::Arc,
    thread,
    time::Duration,
};

use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, LocalMessage, ProxyToLayerMessage, ShareControlEvent,
    ShareReport,
    codec::{SyncDecoder, SyncEncoder},
};
use mirrord_progress::{
    LineMode, MIRRORD_SHARE_PROGRESS_ENV, message_line, messages::LOCAL_SHARE_NOTE,
};

use crate::{
    detour::DetourGuard,
    error::LayerError,
    logging::{report_to_stderr, report_to_stdout},
    proxy_connection::ProxyError,
};

/// How long the layer waits for the internal proxy to confirm the share control connection,
/// while the application can't continue. The internal proxy confirms it right away, or ends the
/// session when the link already failed, so this only bounds the wait on an internal proxy that
/// stopped answering.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);

/// The share control connection of this process. Dropping it closes the connection, which is how
/// the internal proxy learns that the process exited.
#[derive(Debug)]
pub struct ShareControl {
    /// Shared with the printing thread, which reads it. Holding it here keeps its fd open for as
    /// long as this exists, even after the thread stopped, so the application can't get the number
    /// back while [`ShareControl::reset_after_fork`] may still close it.
    socket: Arc<TcpStream>,
}

impl ShareControl {
    /// The printing thread doesn't survive fork, so close only this child's descriptor copy.
    #[cfg(unix)]
    pub fn reset_after_fork(self) {
        let inherited = std::mem::ManuallyDrop::new(self);
        // SAFETY: this descriptor was copied by fork, but the thread that shares it did not, so
        // nothing else in the child uses it. close, unlike shutdown, does not affect the parent's
        // socket connection.
        unsafe {
            libc::close(inherited.socket.as_raw_fd());
        }
    }
}

impl Drop for ShareControl {
    /// Ends the connection without joining the printing thread, which can be blocked writing to
    /// a pipe that nobody reads.
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

/// Attaches a share control connection to the registered layer `layer_id`, and starts the thread
/// that prints what the internal proxy reports on it.
///
/// Returns once the internal proxy confirms the connection, so the application can't listen on
/// a shared port before the internal proxy can reach it.
pub fn start_share_control(
    proxy_addr: SocketAddr,
    layer_id: LayerId,
) -> Result<ShareControl, LayerError> {
    let mode = std::env::var(MIRRORD_SHARE_PROGRESS_ENV)
        .ok()
        .and_then(|value| LineMode::from_name(&value))
        .unwrap_or(LineMode::Simple);
    let socket = TcpStream::connect(proxy_addr).map_err(ProxyError::from)?;
    // Like the request connection, so that the application's own use of low fd numbers can't
    // replace the socket, whose EOF ends this process's layer in the internal proxy.
    #[cfg(unix)]
    let socket = mirrord_intproxy_protocol::codec::move_to_high_fd(socket);
    socket.set_nodelay(true).map_err(ProxyError::from)?;
    socket
        .set_read_timeout(Some(REGISTRATION_TIMEOUT))
        .map_err(ProxyError::from)?;
    SyncEncoder::<LocalMessage<LayerToProxyMessage>, _>::new(&socket)
        .send(&LocalMessage {
            message_id: 0,
            inner: LayerToProxyMessage::OpenShareControl { layer_id },
        })
        .map_err(ProxyError::from)?;
    let first = SyncDecoder::<LocalMessage<ProxyToLayerMessage>, _>::new(&socket)
        .receive()
        .map_err(ProxyError::from)?;
    if matches!(
        first,
        Some(LocalMessage {
            inner: ProxyToLayerMessage::ShareControl(ShareControlEvent::Registered),
            ..
        })
    )
    .not()
    {
        return Err(ProxyError::ProxyFailure(
            "The internal proxy didn't confirm the share control connection.".to_owned(),
        )
        .into());
    }
    socket.set_read_timeout(None).map_err(ProxyError::from)?;
    let socket = Arc::new(socket);
    let reader = Arc::clone(&socket);
    thread::Builder::new()
        .name("mirrord-share".to_owned())
        .spawn(move || {
            let _guard = DetourGuard::new();
            let mut decoder = SyncDecoder::<LocalMessage<ProxyToLayerMessage>, _>::new(&*reader);
            // The thread stops when the internal proxy closes the connection, and leaves ending
            // the application to the internal proxy.
            while let Ok(Some(message)) = decoder.receive() {
                let ProxyToLayerMessage::ShareControl(ShareControlEvent::Report(report)) =
                    message.inner
                else {
                    break;
                };
                let text = match report {
                    ShareReport::Waiting { port } => format!(
                        "Waiting for your application to listen on port {port} to share it."
                    ),
                    ShareReport::Ready { url } => format!("{url}\n{LOCAL_SHARE_NOTE}"),
                    ShareReport::Ended { port } => format!(
                        "The share link ended because your application stopped listening on port {port}. Restart the session to get a new link."
                    ),
                    ShareReport::Failed { message } => {
                        format!("Can't share this session: {message}")
                    }
                };
                let line = message_line(mode, &text);
                match mode {
                    LineMode::Json => report_to_stdout(format_args!("{line}")),
                    LineMode::Simple => report_to_stderr(format_args!("{line}")),
                }
            }
        })
        .map_err(ProxyError::from)?;
    Ok(ShareControl { socket })
}
