use std::net::ToSocketAddrs;

fn main() {
    let domain_x_addrs = ("github.com", 0).to_socket_addrs();
    assert!(domain_x_addrs.is_ok());

    let domain_y_addrs = ("nonexistent-domain", 0).to_socket_addrs();
    #[cfg(target_os = "macos")]
    let error_text =
        "failed to lookup address information: nodename nor servname provided, or not known";
    #[cfg(not(target_os = "macos"))]
    let error_text = "failed to lookup address information: Name or service not known";
    assert_eq!(domain_y_addrs.unwrap_err().to_string(), error_text);
}
