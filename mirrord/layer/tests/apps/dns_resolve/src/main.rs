use std::{ffi::CString, ptr, str};

use libc;

fn get_ip_addresses(node: &str) -> (Vec<String>, i32) {
    println!("Getting addresses for {}", node);
    let node = CString::new(node).unwrap();
    let service = CString::new("http").unwrap();

    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;

    let mut res: *mut libc::addrinfo = ptr::null_mut();
    let result = unsafe { libc::getaddrinfo(node.as_ptr(), service.as_ptr(), &hints, &mut res) };
    let mut current = res;
    let mut addresses = Vec::new();
    while !current.is_null() {
        unsafe {
            let sockaddr = (*current).ai_addr;
            let sockaddr_in = sockaddr as *const libc::sockaddr_in;
            let ip = (*sockaddr_in).sin_addr.s_addr;

            let ipv4_addr = std::net::Ipv4Addr::from(ip);
            let addr_str = ipv4_addr.to_string();

            addresses.push(addr_str);
        }
        current = (unsafe { *current }).ai_next;
    }
    unsafe { libc::freeaddrinfo(res) };
    (addresses, result)
}

fn test_getaddrinfo() {
    let domain_x = "example.com";
    let domain_y = "nonexistent-domain";

    let (addresses_x, result_x) = get_ip_addresses(domain_x);
    assert!(!addresses_x.is_empty(), "Expected addresses for domain X");
    assert!(result_x == 0, "Expected success for domain X");

    let (addresses_y, result_y) = get_ip_addresses(domain_y);
    assert!(addresses_y.is_empty(), "Expected no addresses for domain Y");
    assert!(result_y == libc::EAI_FAIL, "Expected failure for domain Y");
}

fn main() {
    test_getaddrinfo();
}
