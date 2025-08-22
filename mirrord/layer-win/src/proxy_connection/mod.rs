//! Proxy connection code.

use std::net::{SocketAddr, TcpStream};

use crate::error::Error;

#[derive(Debug)]
pub struct ProxyConnection {
    stream: TcpStream,
}

impl ProxyConnection {
    pub fn new(addr: SocketAddr) -> anyhow::Result<Self> {
        // Try to create a connection to the [`addr`].
        // Format should be `<IPV4/IPV6>:<port>`.
        let stream = TcpStream::connect(&addr)
            .map_err(|_| crate::error::Error::UnreachableIntProxyAddr(addr))?;

        dbg!(&stream);

        Ok(Self { stream })
    }
}
