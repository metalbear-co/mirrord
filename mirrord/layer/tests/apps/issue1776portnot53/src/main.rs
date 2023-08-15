use std::{
    alloc::{self, Layout},
    mem::{self, size_of},
    net::{SocketAddr, UdpSocket},
    os::fd::AsRawFd,
};

use libc::{c_void, iovec};
use socket2::SockAddr;

unsafe fn address_from_raw(
    raw_address: *const libc::sockaddr,
    address_length: libc::socklen_t,
) -> Option<SocketAddr> {
    SockAddr::try_init(|storage, len| {
        storage.copy_from_nonoverlapping(raw_address.cast(), 1);
        len.copy_from_nonoverlapping(&address_length, 1);

        Ok(())
    })
    .ok()
    .and_then(|((), address)| address.as_socket())
}

fn main() {
    println!("test issue 1776 port not 53: START");

    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let remote_addr: SocketAddr = "127.0.0.1:7778".parse().unwrap();

    let socket = UdpSocket::bind(local_addr).unwrap();
    let recv_socket = UdpSocket::bind(remote_addr).unwrap();

    let socket_fd = socket.as_raw_fd();
    let recv_socket_fd = recv_socket.as_raw_fd();

    let rawish_remote_addr = SockAddr::from(remote_addr);
    let remote_addr_ptr = rawish_remote_addr.as_ptr() as *mut c_void;
    let remote_addr_len = rawish_remote_addr.len();

    let recv_thread = std::thread::spawn(move || unsafe {
        let mut incoming_msg_hdr: libc::msghdr = mem::zeroed();
        let incoming_name =
            alloc::alloc_zeroed(Layout::array::<u8>(size_of::<libc::sockaddr>()).unwrap())
                as *mut _;
        let incoming_name_len = remote_addr_len;

        incoming_msg_hdr.msg_name = incoming_name;
        incoming_msg_hdr.msg_namelen = incoming_name_len;

        let recv_buffer = &mut vec![0u8; 4];
        let recv_buffer_ptr = recv_buffer.as_mut_ptr() as *mut c_void;
        let mut iov = iovec {
            iov_base: recv_buffer_ptr,
            iov_len: recv_buffer.len(),
        };

        incoming_msg_hdr.msg_iov = &mut iov;
        incoming_msg_hdr.msg_iovlen = 1;

        let amount = libc::recvmsg(recv_socket_fd, &mut incoming_msg_hdr, 0);

        let raw_incoming_addr = incoming_msg_hdr.msg_name as *const libc::sockaddr;
        let raw_incoming_addr_len = incoming_msg_hdr.msg_namelen;
        let incoming_addr = address_from_raw(raw_incoming_addr, raw_incoming_addr_len).unwrap();
        assert_eq!(amount, 4);
        assert_eq!(incoming_addr, local_addr);
    });

    // mirrord should intercept this
    unsafe {
        let mut outgoing_msg_hdr: libc::msghdr = mem::zeroed();

        outgoing_msg_hdr.msg_name = remote_addr_ptr;
        outgoing_msg_hdr.msg_namelen = remote_addr_len;

        let bytes = &mut [0u8, 1, 2, 3];
        let bytes_ptr = bytes.as_mut_ptr() as *mut c_void;
        let mut iov = iovec {
            iov_base: bytes_ptr,
            iov_len: 4,
        };

        outgoing_msg_hdr.msg_iov = &mut iov;
        outgoing_msg_hdr.msg_iovlen = 1;

        let amount = libc::sendmsg(socket_fd, &outgoing_msg_hdr, 0);
        assert_eq!(amount, 4);
    }

    recv_thread.join().unwrap();

    println!("test issue 1776 port not 53: SUCCESS");
}
