use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
};

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcpSteal, NewTcpConnection, TcpClose, TcpData},
    ConnectionId, Port,
};
use rand::distributions::{Alphanumeric, DistString};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, log::warn};

use crate::{
    error::{AgentError, Result},
    runtime::set_namespace,
};

#[cfg_attr(test, mockall::automock)]
trait IPTables {
    fn create_chain(&self, name: &str) -> Result<()>;
    fn remove_chain(&self, name: &str) -> Result<()>;

    fn add_rule(&self, chain: &str, rule: &str) -> Result<()>;
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()>;
    fn list_rules(&self, chain: &str) -> Result<Vec<String>>;
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()>;
}

impl IPTables for iptables::IPTables {
    fn create_chain(&self, name: &str) -> Result<()> {
        self.new_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.append(IPTABLES_TABLE_NAME, name, "-j RETURN")
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    fn remove_chain(&self, name: &str) -> Result<()> {
        self.flush_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.delete_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    fn add_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.append(IPTABLES_TABLE_NAME, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()> {
        self.insert(IPTABLES_TABLE_NAME, chain, rule, index)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    fn list_rules(&self, chain: &str) -> Result<Vec<String>> {
        self.list(IPTABLES_TABLE_NAME, chain)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.delete(IPTABLES_TABLE_NAME, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }
}

/// Wrapper struct for IPTables so it flushes on drop.
struct SafeIpTables<IPT: IPTables> {
    inner: IPT,
    chain_name: String,
    formatter: IPTableFormatter,
}

const IPTABLES_TABLE_NAME: &str = "nat";
/// Wrapper for using iptables. This creates a a new chain on creation and deletes it on drop.
/// The way it works is that it adds a chain, then adds a rule to the chain that returns to the
/// original chain (fallback) and adds a rule in the "PREROUTING" table that jumps to the new chain.
/// Connections will go then PREROUTING -> OUR_CHAIN -> IF MATCH REDIRECT -> IF NOT MATCH FALLBACK
/// -> ORIGINAL_CHAIN
impl<IPT> SafeIpTables<IPT>
where
    IPT: IPTables,
{
    pub fn new(ipt: IPT) -> Result<Self> {
        let formatter = IPTableFormatter::detect(&ipt)?;

        let random_string = Alphanumeric.sample_string(&mut rand::thread_rng(), 5);
        let chain_name = format!("MIRRORD_REDIRECT_{}", random_string);

        ipt.create_chain(&chain_name)?;

        ipt.add_rule(formatter.entrypoint(), &format!("-j {}", chain_name))?;

        Ok(Self {
            inner: ipt,
            chain_name,
            formatter,
        })
    }

    pub fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner.insert_rule(
            &self.chain_name,
            &self.formatter.redirect_rule(redirected_port, target_port),
            1,
        )
    }

    pub fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner.remove_rule(
            &self.chain_name,
            &self.formatter.redirect_rule(redirected_port, target_port),
        )
    }
}

impl<IPT> Drop for SafeIpTables<IPT>
where
    IPT: IPTables,
{
    fn drop(&mut self) {
        self.inner
            .remove_rule(
                self.formatter.entrypoint(),
                &format!("-j {}", self.chain_name),
            )
            .unwrap();

        self.inner.remove_chain(&self.chain_name).unwrap();
    }
}

enum IPTableFormatter {
    Normal,
    Linkerd,
}

impl IPTableFormatter {
    fn detect<IPT: IPTables>(ipt: &IPT) -> Result<Self> {
        let output = ipt.list_rules("OUTPUT")?;

        if output
            .iter()
            .any(|rule| rule.contains("-j PROXY_INIT_OUTPUT"))
        {
            Ok(IPTableFormatter::Linkerd)
        } else {
            Ok(IPTableFormatter::Normal)
        }
    }

    fn entrypoint(&self) -> &str {
        match self {
            IPTableFormatter::Normal => "PREROUTING",
            IPTableFormatter::Linkerd => "OUTPUT",
        }
    }

    fn redirect_rule(&self, redirected_port: Port, target_port: Port) -> String {
        let redirect_rule = format!(
            "-m tcp -p tcp --dport {} -j REDIRECT --to-ports {}",
            redirected_port, target_port
        );

        match self {
            IPTableFormatter::Normal => redirect_rule,
            IPTableFormatter::Linkerd => format!("-o lo {}", redirect_rule),
        }
    }
}

pub struct StealWorker {
    pub sender: Sender<DaemonTcp>,
    iptables: SafeIpTables<iptables::IPTables>,
    ports: HashSet<Port>,
    listen_port: Port,
    write_streams: HashMap<ConnectionId, WriteHalf<TcpStream>>,
    read_streams: StreamMap<ConnectionId, ReaderStream<ReadHalf<TcpStream>>>,
    connection_index: u64,
}

impl StealWorker {
    pub fn new(sender: Sender<DaemonTcp>, listen_port: Port) -> Result<Self> {
        Ok(Self {
            sender,
            iptables: SafeIpTables::new(iptables::new(false).unwrap())?,
            ports: HashSet::default(),
            listen_port,
            write_streams: HashMap::default(),
            read_streams: StreamMap::default(),
            connection_index: 0,
        })
    }

    pub async fn handle_loop(
        &mut self,
        mut rx: Receiver<LayerTcpSteal>,
        listener: TcpListener,
    ) -> Result<()> {
        loop {
            select! {
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        self.handle_client_message(msg).await?;
                    } else {
                        debug!("rx closed, breaking");
                        break;
                    }
                },
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, address)) => {
                            self.handle_incoming_connection(stream, address).await?;
                        },
                        Err(err) => {
                            error!("accept error {err:?}");
                            break;
                        }
                    }
                },
                message = self.next() => {
                    if let Some(message) = message {
                        self.sender.send(message).await?;
                    }
                }
            }
        }
        debug!("TCP Stealer exiting");
        Ok(())
    }

    pub async fn handle_client_message(&mut self, message: LayerTcpSteal) -> Result<()> {
        use LayerTcpSteal::*;
        match message {
            PortSubscribe(port) => {
                if self.ports.contains(&port) {
                    warn!("Port {port:?} is already subscribed");
                    Ok(())
                } else {
                    debug!("adding redirect rule");
                    self.iptables.add_redirect(port, self.listen_port)?;
                    self.ports.insert(port);
                    self.sender.send(DaemonTcp::Subscribed).await?;
                    debug!("sent subscribed");
                    Ok(())
                }
            }
            ConnectionUnsubscribe(connection_id) => {
                info!("Closing connection {connection_id:?}");
                self.write_streams.remove(&connection_id);
                self.read_streams.remove(&connection_id);
                Ok(())
            }
            PortUnsubscribe(port) => {
                if self.ports.remove(&port) {
                    self.iptables.remove_redirect(port, self.listen_port)
                } else {
                    warn!("removing unsubscribed port {port:?}");
                    Ok(())
                }
            }

            Data(data) => {
                if let Some(stream) = self.write_streams.get_mut(&data.connection_id) {
                    stream.write_all(&data.bytes[..]).await?;
                    Ok(())
                } else {
                    warn!(
                        "Trying to send data to closed connection {:?}",
                        data.connection_id
                    );
                    Ok(())
                }
            }
        }
    }

    pub async fn handle_incoming_connection(
        &mut self,
        stream: TcpStream,
        address: SocketAddr,
    ) -> Result<()> {
        let real_addr = orig_dst::orig_dst_addr(&stream)?;
        if !self.ports.contains(&real_addr.port()) {
            return Err(AgentError::UnexpectedConnection(real_addr.port()));
        }
        let connection_id = self.connection_index;
        self.connection_index += 1;

        let (read_half, write_half) = tokio::io::split(stream);
        self.write_streams.insert(connection_id, write_half);
        self.read_streams
            .insert(connection_id, ReaderStream::new(read_half));

        let new_connection = DaemonTcp::NewConnection(NewTcpConnection {
            connection_id,
            destination_port: real_addr.port(),
            source_port: address.port(),
            address: address.ip(),
        });
        self.sender.send(new_connection).await?;
        debug!("sent new connection");
        Ok(())
    }

    pub async fn next(&mut self) -> Option<DaemonTcp> {
        let (connection_id, value) = self.read_streams.next().await?;
        match value {
            Some(Ok(bytes)) => Some(DaemonTcp::Data(TcpData {
                connection_id,
                bytes: bytes.to_vec(),
            })),
            Some(Err(err)) => {
                error!("connection id {connection_id:?} read error: {err:?}");
                None
            }
            None => Some(DaemonTcp::Close(TcpClose { connection_id })),
        }
    }
}

pub async fn steal_worker(
    rx: Receiver<LayerTcpSteal>,
    tx: Sender<DaemonTcp>,
    pid: Option<u64>,
) -> Result<()> {
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace)?;
    }
    debug!("preparing steal");
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let listen_port = listener.local_addr()?.port();
    let mut worker = StealWorker::new(tx, listen_port)?;
    debug!("finished preparing steal");
    worker.handle_loop(rx, listener).await?;
    debug!("steal exiting");

    Ok(())
}

// orig_dst borrowed from linkerd2-proxy
// https://github.com/linkerd/linkerd2-proxy/blob/main/linkerd/proxy/transport/src/orig_dst.rs
// copyright 2018 the linkerd2-proxy authors
mod orig_dst {
    use std::{io, net::SocketAddr};

    use tokio::net::TcpStream;

    #[cfg(target_os = "linux")]
    #[allow(unsafe_code)]
    pub fn orig_dst_addr(sock: &TcpStream) -> io::Result<SocketAddr> {
        use std::os::unix::io::AsRawFd;
        let fd = sock.as_raw_fd();
        unsafe { linux::so_original_dst(fd) }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn orig_dst_addr(_: &TcpStream) -> io::Result<SocketAddr> {
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

        pub unsafe fn so_original_dst(fd: RawFd) -> io::Result<SocketAddr> {
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
        fn mk_addr(
            storage: &libc::sockaddr_storage,
            len: libc::socklen_t,
        ) -> io::Result<SocketAddr> {
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
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;

    use super::*;

    #[test]
    fn default() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        let ipt = SafeIpTables::new(mock).expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).is_ok());

        assert!(ipt.remove_redirect(69, 420).is_ok());
    }

    #[test]
    fn linkerd() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec!["-j PROXY_INIT_OUTPUT".to_owned()]));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        let ipt = SafeIpTables::new(mock).expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).is_ok());

        assert!(ipt.remove_redirect(69, 420).is_ok());
    }
}
