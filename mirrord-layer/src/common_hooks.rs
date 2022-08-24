use frida_gum::interceptor::Interceptor;

#[cfg(target_os = "linux")]
use crate::common_hooks::hooks::{FnDup3, FN_DUP3};
use crate::{
    common_hooks::hooks::{FnDup, FnDup2, FnFcntl, FN_DUP, FN_DUP2, FN_FCNTL},
    replace,
};

mod ops {
    use std::os::unix::prelude::RawFd;

    use libc::c_int;

    use crate::{
        error::{HookError, HookResult as Result},
        file::OPEN_FILES,
        socket::SOCKETS,
        ENABLED_FILE_OPS,
    };

    pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<()> {
        match cmd {
            libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup(orig_fd, fcntl_fd),
            _ => Ok(()),
        }
    }

    pub(super) fn dup(fd: c_int, dup_fd: i32) -> Result<()> {
        {
            let mut sockets = SOCKETS.lock()?;
            if let Some(socket) = sockets.get(&fd).cloned() {
                sockets.insert(dup_fd as RawFd, socket);
                return Ok(());
            }
        }
        if *ENABLED_FILE_OPS.get().unwrap() {
            let mut files = OPEN_FILES.lock()?;
            if let Some(file) = files.get(&fd).cloned() {
                files.insert(dup_fd as RawFd, file);
                return Ok(());
            }
        }
        Err(HookError::LocalFDNotFound(fd))
    }
}

mod hooks {
    use libc::c_int;
    use mirrord_macro::{hook_fn, hook_guard_fn};
    use tracing::trace;

    use super::ops::*;
    use crate::error::HookError;

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
                        HookError::LocalFDNotFound(_) => dup_result,
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
                        HookError::LocalFDNotFound(_) => dup2_result,
                        _ => fail.into(),
                    });

            trace!("dup2_detour -> result {:#?}", result);
            result
        }
    }

    #[cfg(target_os = "linux")]
    #[hook_guard_fn]
    pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
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
                        HookError::LocalFDNotFound(_) => dup3_result,
                        _ => fail.into(),
                    });

            trace!("dup3_detour -> result {:#?}", result);
            result
        }
    }
}

pub(crate) unsafe fn enable_common_hooks(interceptor: &mut Interceptor) {
    let _ = replace!(interceptor, "fcntl", hooks::fcntl_detour, FnFcntl, FN_FCNTL);
    let _ = replace!(interceptor, "dup", hooks::dup_detour, FnDup, FN_DUP);
    let _ = replace!(interceptor, "dup2", hooks::dup2_detour, FnDup2, FN_DUP2);
    #[cfg(target_os = "linux")]
    let _ = replace!(interceptor, "dup3", hooks::dup3_detour, FnDup3, FN_DUP3);
}
