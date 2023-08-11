#![allow(unused)]
use std::{
    alloc::{self, Layout},
    mem::{self, size_of},
    net::{SocketAddr, UdpSocket},
    os::fd::AsRawFd,
};

use libc::{c_void, iovec, msghdr};
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
    let addr1: SocketAddr = "127.0.0.1:7777".parse().unwrap();
    let addr2: SocketAddr = "127.0.0.1:8888".parse().unwrap();

    let udp_1 = UdpSocket::bind(addr1).unwrap();
    // let udp_2 = UdpSocket::bind(addr2).unwrap();

    // udp_1.connect(udp_2.local_addr().unwrap()).unwrap();

    let udp_1_fd = udp_1.as_raw_fd();

    // let receiver = std::thread::spawn(move || {
    //     let udp_2_fd = udp_2.as_raw_fd();

    //     unsafe {
    //         let mut msg_hdr: msghdr = mem::zeroed();
    //         let name =
    //             alloc::alloc_zeroed(Layout::array::<u8>(size_of::<libc::sockaddr>()).unwrap())
    //                 as *mut _;
    //         let name_len = 14;

    //         msg_hdr.msg_name = name;
    //         msg_hdr.msg_namelen = name_len;

    //         let bytes = &mut [0u8, 0, 0, 0];
    //         let bytes_ptr = bytes.as_mut_ptr() as *mut c_void;
    //         let mut iov = iovec {
    //             iov_base: bytes_ptr,
    //             iov_len: 4,
    //         };

    //         msg_hdr.msg_iov = &mut iov;
    //         msg_hdr.msg_iovlen = 1;

    //         let amount = libc::recvmsg(udp_2_fd, &mut msg_hdr, 0);
    //         println!("received some data {amount:#?}");

    //         let raw_from = msg_hdr.msg_name as *const libc::sockaddr;
    //         let raw_length = msg_hdr.msg_namelen;
    //         let from = address_from_raw(raw_from, raw_length);
    //         println!("received some data {amount:#?} from {from:#?}");
    //     }
    // });

    unsafe {
        let mut msg_hdr: libc::msghdr = mem::zeroed();
        let addr2: SocketAddr = "34.231.133.117:27017".parse().unwrap();

        let to = SockAddr::from(addr2);
        let to_ptr = to.as_ptr() as *mut c_void;
        let to_length = to.len();

        msg_hdr.msg_name = to_ptr;
        msg_hdr.msg_namelen = to_length;

        let bytes = &mut [0u8, 1, 2, 3];
        let bytes_ptr = bytes.as_mut_ptr() as *mut c_void;
        let mut iov = iovec {
            iov_base: bytes_ptr,
            iov_len: 4,
        };

        msg_hdr.msg_iov = &mut iov;
        msg_hdr.msg_iovlen = 1;

        libc::sendmsg(udp_1_fd, &msg_hdr, 0);
        println!("sent {bytes:#?} to {:#?}", to.as_socket());
    }

    // receiver.join().unwrap();
}
