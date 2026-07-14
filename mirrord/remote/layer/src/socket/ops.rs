use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    ptr,
    sync::{Mutex, OnceLock},
};

use libc::{sockaddr, socklen_t};
use mirrord_layer_lib::{detour::Detour, error::HookError};
use nix::sys::socket::SockaddrStorage;
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

pub(crate) fn connect_placeholder_socket(
    address: SocketAddr,
    accepted_fd: &OwnedFd,
) -> io::Result<OwnedFd> {
    let (domain, type_, protocol) = match address {
        SocketAddr::V4(_) => (libc::AF_INET, libc::SOCK_STREAM, 0),
        SocketAddr::V6(_) => (libc::AF_INET6, libc::SOCK_STREAM, 0),
    };

    let raw_fd = unsafe { libc::socket(domain, type_, protocol) };
    if raw_fd == -1 {
        return Err(io::Error::last_os_error());
    }

    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

    let address = SockaddrStorage::from(address);
    nix::sys::socket::connect(fd.as_raw_fd(), &address).map_err(io::Error::from)?;

    let fd_flags = unsafe { libc::fcntl(accepted_fd.as_raw_fd(), libc::F_GETFD) };
    if fd_flags == -1 {
        return Err(io::Error::last_os_error());
    }

    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, fd_flags) } == -1 {
        return Err(io::Error::last_os_error());
    }

    let status_flags = unsafe { libc::fcntl(accepted_fd.as_raw_fd(), libc::F_GETFL) };
    if status_flags == -1 {
        return Err(io::Error::last_os_error());
    }

    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, status_flags) } == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(fd)
}

pub(crate) fn claim_placeholder_socket(
    accepted_fd: &OwnedFd,
    placeholder_address: SocketAddr,
    local_address: SocketAddr,
    peer_address: SocketAddr,
) -> io::Result<OwnedFd> {
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

pub(crate) fn socket_addr_from_fd(fd: RawFd) -> io::Result<SocketAddr> {
    let address = nix::sys::socket::getsockname::<SockaddrStorage>(fd).map_err(io::Error::other)?;
    socket_addr_from_storage(address)
}

pub(crate) fn socket_peer_addr_from_fd(fd: RawFd) -> io::Result<SocketAddr> {
    let address = nix::sys::socket::getpeername::<SockaddrStorage>(fd).map_err(io::Error::other)?;
    socket_addr_from_storage(address)
}

pub(crate) fn socket_addr_from_storage(address: SockaddrStorage) -> io::Result<SocketAddr> {
    if let Some(ipv4) = address.as_sockaddr_in() {
        Ok(SocketAddrV4::from(*ipv4).into())
    } else if let Some(ipv6) = address.as_sockaddr_in6() {
        Ok(SocketAddrV6::from(*ipv6).into())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected an IPv4 or IPv6 socket address",
        ))
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
