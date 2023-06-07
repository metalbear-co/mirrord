use std::net::ToSocketAddrs;

fn main() {
    let domain_x_addrs = ("github.com", 0).to_socket_addrs();
    assert!(domain_x_addrs.is_ok());

    let domain_y_addrs = ("nonexistent-domain", 0).to_socket_addrs();
    assert_eq!(
        domain_y_addrs.unwrap_err().to_string(),
        "failed to lookup address information: Non-recoverable failure in name resolution"
    );
}
