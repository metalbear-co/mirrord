use frida_gum::interceptor::Interceptor;
use mirrord_protocol::{AddrInfoHint, AddrInfoInternal, RemoteResult};
use tokio::sync::oneshot;

use crate::{error::LayerError, HOOK_SENDER, file::HookMessageFile, tcp::HookMessageTcp, replace};


pub(crate) fn blocking_send_hook_message(message: HookMessage) -> Result<(), LayerError> {
    unsafe {
        HOOK_SENDER
            .as_ref()
            .ok_or(LayerError::EmptyHookSender)
            .and_then(|hook_sender| hook_sender.blocking_send(message).map_err(Into::into))
    }
}

pub(crate) type ResponseChannel<T> = oneshot::Sender<RemoteResult<T>>;

#[derive(Debug)]
pub struct GetAddrInfoHook {
    pub(crate) node: Option<String>,
    pub(crate) service: Option<String>,
    pub(crate) hints: Option<AddrInfoHint>,
    pub(crate) hook_channel_tx: ResponseChannel<Vec<AddrInfoInternal>>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Tcp(HookMessageTcp),
    File(HookMessageFile),
    GetAddrInfoHook(GetAddrInfoHook),
}


mod common_ops {
    use std::os::unix::prelude::RawFd;

    use libc::c_int;

    use crate::{error::LayerError, file::OPEN_FILES, socket::SOCKETS};

    pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<(), LayerError> {
        match cmd {
            libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup(orig_fd, fcntl_fd),
            _ => Ok(()),
        }
    }

    pub(super) fn dup(fd: c_int, dup_fd: i32) -> Result<(), LayerError> {
        if let Some(socket) = SOCKETS.lock()?.get(&fd) {
            SOCKETS.lock()?.insert(dup_fd as RawFd, socket.clone());
        } else if let Some(file) = OPEN_FILES.lock()?.get(&fd) {
            OPEN_FILES.lock()?.insert(dup_fd as RawFd, file.clone());
        } else {
            return Err(LayerError::LocalFDNotFound(fd));
        }
        Ok(())
    }
}


mod common_hooks {
    use libc::c_int;
    use tracing::trace;    
    use crate::error::LayerError;

    use super::common_ops::*;

    /// https://github.com/metalbear-co/mirrord/issues/184
    #[hook_fn]
    pub(super) unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, mut arg: ...) -> c_int {
        trace!("fcntl_detour -> fd {:#?} | cmd {:#?}", fd, cmd);

        let arg = arg.arg::<usize>();
        let fcntl_result = FN_FCNTL(fd, cmd, arg);
        let guard = crate::DetourGuard::new();
        if guard.is_none() {
            return fcntl_result;
        }

        if fcntl_result == -1 {
            fcntl_result
        } else {
            let (Ok(result) | Err(result)) = fcntl(fd, cmd, fcntl_result)
                .map(|()| fcntl_result)
                .map_err(From::from);

            trace!("fcntl_detour -> result {:#?}", result);
            result
        }
    }

    #[hook_guard_fn]
    pub(super) unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
        trace!("dup_detour -> fd {:#?}", fd);

        let dup_result = FN_DUP(fd);

        if dup_result == -1 {
            dup_result
        } else {
            let (Ok(result) | Err(result)) =
                dup(fd, dup_result)
                    .map(|()| dup_result)
                    .map_err(|fail| match fail {
                        LayerError::LocalFDNotFound(_) => dup_result,
                        _ => fail.into(),
                    });

            trace!("dup_detour -> result {:#?}", result);
            result
        }
    }

    #[hook_guard_fn]
    pub(super) unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
        trace!("dup2_detour -> oldfd {:#?} | newfd {:#?}", oldfd, newfd);

        if oldfd == newfd {
            return newfd;
        }

        let dup2_result = FN_DUP2(oldfd, newfd);

        if dup2_result == -1 {
            dup2_result
        } else {
            let (Ok(result) | Err(result)) =
                dup(oldfd, dup2_result)
                    .map(|()| dup2_result)
                    .map_err(|fail| match fail {
                        LayerError::LocalFDNotFound(_) => dup2_result,
                        _ => fail.into(),
                    });

            trace!("dup2_detour -> result {:#?}", result);
            result
        }
    }

    #[cfg(target_os = "linux")]
    #[hook_guard_fn]
    pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {        
        use crate::error::LayerError;

        trace!(
            "dup3_detour -> oldfd {:#?} | newfd {:#?} | flags {:#?}",
            oldfd,
            newfd,
            flags
        );

        let dup3_result = FN_DUP3(oldfd, newfd, flags);

        if dup3_result == -1 {
            dup3_result
        } else {
            let (Ok(result) | Err(result)) =
                dup(oldfd, dup3_result)
                    .map(|()| dup3_result)
                    .map_err(|fail| match fail {
                        LayerError::LocalFDNotFound(_) => dup3_result,
                        _ => fail.into(),
                    });

            trace!("dup3_detour -> result {:#?}", result);
            result
        }
    }
}

pub(crate) unsafe fn enable_common_hooks(interceptor: &mut Interceptor) {
    let _ = replace!(interceptor, "fcntl", common_hooks::fcntl_detour, FnFcntl, FN_FCNTL);
    let _ = replace!(interceptor, "dup", common_hooks::dup_detour, FnDup, FN_DUP);
    let _ = replace!(interceptor, "dup2", common_hooks::dup2_detour, FnDup2, FN_DUP2);
    #[cfg(target_os = "linux")]
    let _ = replace!(interceptor, "dup3", common_hooks::dup3_detour, FnDup3, FN_DUP3);
}