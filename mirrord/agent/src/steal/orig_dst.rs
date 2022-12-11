/// orig_dst borrowed from linkerd2-proxy
/// https://github.com/linkerd/linkerd2-proxy/blob/main/linkerd/proxy/transport/src/orig_dst.rs
/// copyright 2018 the linkerd2-proxy authors
use std::{io, net::SocketAddr};

use tokio::net::TcpStream;

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
#[tracing::instrument(level = "trace")]
pub(super) fn orig_dst_addr(sock: &TcpStream) -> io::Result<SocketAddr> {
    use std::os::unix::io::AsRawFd;
    let fd = sock.as_raw_fd();
    unsafe { linux::so_original_dst(fd) }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn orig_dst_addr(_: &TcpStream) -> io::Result<SocketAddr> {
    Err(io::Error::new(
        io::ErrorKind::Other,
        "SO_ORIGINAL_DST not supported on this operating system",
    ))
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod linux {
    use std::{
        io, mem,
        net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
        os::unix::io::RawFd,
    };

    use tracing::warn;

    pub(super) unsafe fn so_original_dst(fd: RawFd) -> io::Result<SocketAddr> {
        let mut sockaddr: libc::sockaddr_storage = mem::zeroed();
        let mut socklen: libc::socklen_t = mem::size_of::<libc::sockaddr_storage>() as u32;

        let ret = libc::getsockopt(
            fd,
            libc::SOL_IP,
            libc::SO_ORIGINAL_DST,
            &mut sockaddr as *mut _ as *mut _,
            &mut socklen as *mut _ as *mut _,
        );
        if ret != 0 {
            let e = io::Error::last_os_error();
            warn!("failed to read SO_ORIGINAL_DST: {:?}", e);
            return Err(e);
        }

        mk_addr(&sockaddr, socklen)
    }

    // Borrowed with love from net2-rs
    // https://github.com/rust-lang-nursery/net2-rs/blob/1b4cb4fb05fbad750b271f38221eab583b666e5e/src/socket.rs#L103
    //
    // Copyright (c) 2014 The Rust Project Developers
    fn mk_addr(storage: &libc::sockaddr_storage, len: libc::socklen_t) -> io::Result<SocketAddr> {
        match storage.ss_family as libc::c_int {
            libc::AF_INET => {
                assert!(len as usize >= mem::size_of::<libc::sockaddr_in>());

                let sa = {
                    let sa = storage as *const _ as *const libc::sockaddr_in;
                    unsafe { *sa }
                };

                let bits = ntoh32(sa.sin_addr.s_addr);
                let ip = Ipv4Addr::new(
                    (bits >> 24) as u8,
                    (bits >> 16) as u8,
                    (bits >> 8) as u8,
                    bits as u8,
                );
                let port = sa.sin_port;
                Ok(SocketAddr::V4(SocketAddrV4::new(ip, ntoh16(port))))
            }
            libc::AF_INET6 => {
                assert!(len as usize >= mem::size_of::<libc::sockaddr_in6>());

                let sa = {
                    let sa = storage as *const _ as *const libc::sockaddr_in6;
                    unsafe { *sa }
                };

                let arr = sa.sin6_addr.s6_addr;
                let ip = Ipv6Addr::new(
                    (arr[0] as u16) << 8 | (arr[1] as u16),
                    (arr[2] as u16) << 8 | (arr[3] as u16),
                    (arr[4] as u16) << 8 | (arr[5] as u16),
                    (arr[6] as u16) << 8 | (arr[7] as u16),
                    (arr[8] as u16) << 8 | (arr[9] as u16),
                    (arr[10] as u16) << 8 | (arr[11] as u16),
                    (arr[12] as u16) << 8 | (arr[13] as u16),
                    (arr[14] as u16) << 8 | (arr[15] as u16),
                );

                let port = sa.sin6_port;
                let flowinfo = sa.sin6_flowinfo;
                let scope_id = sa.sin6_scope_id;
                Ok(SocketAddr::V6(SocketAddrV6::new(
                    ip,
                    ntoh16(port),
                    flowinfo,
                    scope_id,
                )))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid argument",
            )),
        }
    }

    fn ntoh16(i: u16) -> u16 {
        <u16>::from_be(i)
    }

    fn ntoh32(i: u32) -> u32 {
        <u32>::from_be(i)
    }
}
