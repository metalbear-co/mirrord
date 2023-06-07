use std::net::ToSocketAddrs;

fn main() {
    let domain_x_addrs = ("github.com", 0).to_socket_addrs();
    assert!(domain_x_addrs.is_ok());

    let domain_y_addrs = ("nonexistent-domain", 0).to_socket_addrs();
    // we are asserting for libc::EAI_FAIL here, but we cant get the underlying error code
    // however, it seems that libc::EAI_FAIL is mapped to the message asserted below
    // refer: https://github.com/rust-lang/rust/blob/master/library/std/src/sys/solid/net.rs#L154
    assert_eq!(
        domain_y_addrs.unwrap_err().to_string(),
        "failed to lookup address information: Non-recoverable failure in name resolution"
    );
}
