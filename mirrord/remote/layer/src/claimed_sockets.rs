use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    ptr,
    sync::{Mutex, OnceLock},
};

use libc::{sockaddr, socklen_t};
use mirrord_layer_lib::{detour::Detour, error::HookError};
use nix::{
    fcntl::{FcntlArg, FdFlag, OFlag, fcntl},
    sys::socket::{AddressFamily, SockFlag, SockProtocol, SockType, SockaddrStorage, socket},
};
use socket2::SockAddr;

#[derive(Clone, Copy)]
pub(crate) struct ClaimedSocket {
    pub(crate) local_address: SocketAddr,
    pub(crate) peer_address: SocketAddr,
}

static CLAIMED_SOCKETS: OnceLock<Mutex<HashMap<RawFd, ClaimedSocket>>> = OnceLock::new();

pub(crate) fn claimed_sockets() -> &'static Mutex<HashMap<RawFd, ClaimedSocket>> {
    CLAIMED_SOCKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn claimed_socket(fd: RawFd) -> Option<ClaimedSocket> {
    claimed_sockets()
        .lock()
        .expect("claimed socket lock failed")
        .get(&fd)
        .copied()
}

pub(crate) fn remove_claimed_socket(fd: RawFd) {
    claimed_sockets()
        .lock()
        .expect("claimed socket lock failed")
        .remove(&fd);
}

pub(crate) fn duplicate_claimed_socket(oldfd: RawFd, newfd: RawFd) {
    if oldfd == newfd {
        return;
    }

    let mut claimed_sockets = claimed_sockets()
        .lock()
        .expect("claimed socket lock failed");

    claimed_sockets.remove(&newfd);
    if let Some(socket) = claimed_sockets.get(&oldfd).copied() {
        claimed_sockets.insert(newfd, socket);
    }
}

pub(crate) fn connect_placeholder_socket(
    address: SocketAddr,
    accepted_fd: &OwnedFd,
) -> nix::Result<OwnedFd> {
    let family = match address {
        SocketAddr::V4(_) => AddressFamily::Inet,
        SocketAddr::V6(_) => AddressFamily::Inet6,
    };
    let fd = socket(
        family,
        SockType::Stream,
        SockFlag::empty(),
        Some(SockProtocol::Tcp),
    )?;

    let address = SockaddrStorage::from(address);
    nix::sys::socket::connect(fd.as_raw_fd(), &address)?;

    let fd_flags = fcntl(accepted_fd, FcntlArg::F_GETFD)?;
    fcntl(&fd, FcntlArg::F_SETFD(FdFlag::from_bits_retain(fd_flags)))?;

    let status_flags = fcntl(accepted_fd, FcntlArg::F_GETFL)?;
    fcntl(
        &fd,
        FcntlArg::F_SETFL(OFlag::from_bits_retain(status_flags)),
    )?;

    Ok(fd)
}

pub(crate) fn claim_placeholder_socket(
    accepted_fd: &OwnedFd,
    placeholder_address: SocketAddr,
    local_address: SocketAddr,
    peer_address: SocketAddr,
) -> nix::Result<OwnedFd> {
    let placeholder_fd = connect_placeholder_socket(placeholder_address, accepted_fd)?;

    claimed_sockets()
        .lock()
        .expect("claimed socket lock failed")
        .insert(
            placeholder_fd.as_raw_fd(),
            ClaimedSocket {
                local_address,
                peer_address,
            },
        );

    Ok(placeholder_fd)
}

pub(crate) fn socket_addr_from_fd(fd: RawFd) -> nix::Result<SocketAddr> {
    let address = nix::sys::socket::getsockname::<SockaddrStorage>(fd)?;
    socket_addr_from_storage(address)
}

pub(crate) fn socket_peer_addr_from_fd(fd: RawFd) -> nix::Result<SocketAddr> {
    let address = nix::sys::socket::getpeername::<SockaddrStorage>(fd)?;
    socket_addr_from_storage(address)
}

pub(crate) fn socket_addr_from_storage(address: SockaddrStorage) -> nix::Result<SocketAddr> {
    if let Some(ipv4) = address.as_sockaddr_in() {
        Ok(SocketAddrV4::from(*ipv4).into())
    } else if let Some(ipv6) = address.as_sockaddr_in6() {
        Ok(SocketAddrV6::from(*ipv6).into())
    } else {
        Err(nix::errno::Errno::EINVAL)
    }
}

pub(crate) fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SockAddr,
) -> Detour<i32> {
    let result = if address.is_null() {
        Ok(0)
    } else if address_len.is_null() {
        Err(HookError::NullPointer)
    } else {
        unsafe {
            let len = std::cmp::min(*address_len as usize, new_address.len() as usize);
            ptr::copy_nonoverlapping(new_address.as_ptr() as *const u8, address as *mut u8, len);
            *address_len = new_address.len();
        }

        Ok(0)
    };

    match result {
        Ok(value) => Detour::Success(value),
        Err(error) => Detour::Error(error),
    }
}
