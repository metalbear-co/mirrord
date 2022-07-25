use std::{collections::{HashSet, HashMap}, fmt::format, net::SocketAddr, path::PathBuf};

use drain::Watch;
use iptables;
use mirrord_protocol::{
    tcp::{ClientStealTcp, DaemonTcp, LayerTcp, NewTcpConnection},
    Port, ConnectionID,
};
use rand::distributions::{Alphanumeric, DistString};
use tokio::{
    io::{copy_bidirectional, duplex, DuplexStream, WriteHalf, ReadHalf},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_stream::StreamMap;
use tokio_util::io::ReaderStream;
use tracing::{debug, error};

use crate::{
    error::{Result, AgentError},
    runtime::{set_namespace},
};

struct SafeIpTables {
    inner: iptables::IPTables,
    chain_name: String,
}

struct PacketData(Vec<u8>);

const IPTABLES_TABLE_NAME: &str = "nat";
const MIRRORD_CHAIN_NAME: &str = "MIRRORD_REDIRECT";

impl SafeIpTables {
    pub fn new() -> Result<Self> {
        let ipt = iptables::new(false).unwrap();
        let random_string = Alphanumeric.sample_string(&mut rand::thread_rng(), 5);
        let chain_name = format!("MIRRORD_REDIRECT_{}", random_string);
        ipt.new_chain(IPTABLES_TABLE_NAME, chain_name)?;
        ipt.append(IPTABLES_TABLE_NAME, chain_name, "-j RETURN")?;
        ipt.append(
            IPTABLES_TABLE_NAME,
            "PREROUTING",
            format!("-j {}", chain_name),
        )?;
        Ok(Self {
            inner: ipt,
            chain_name,
        })
    }

    pub fn add_redirect(
        &mut self,
        redirected_port: Port,
        target_port: Port,
    ) -> Result<()> {
        let rule = format!(
            "-p tcp -m tcp --dport {} -j REDIRECT --to-ports {}",
            redirected_port, target_port
        );
        self.inner
            .insert(IPTABLES_TABLE_NAME, self.chain_name, &rule, 0)
    }
}

impl Drop for SafeIpTables {
    fn drop(&mut self) {
        let _ = self
            .inner
            .delete(
                IPTABLES_TABLE_NAME,
                "PREROUTING",
                format!("-j {}", self.chain_name),
            )
            .unwrap();
        let _ = self
            .inner
            .delete_chain(IPTABLES_TABLE_NAME, self.chain_name);
    }
}

struct StealWorker {
    pub sender: Sender<DaemonTcp>,
    iptables: SafeIpTables,
    ports: HashSet<Port>,
    listen_port: Port,
    write_streams: HashMap<ConnectionID, WriteHalf<TcpStream>>,
    read_streams: StreamMap<ConnectionID, ReaderStream<ReadHalf<TcpStream>>>
}

impl StealWorker {
    pub fn new(sender: Sender<DaemonTcp>, listen_port: Port) -> Result<Self> {
        Ok(Self {
            sender,
            iptables: SafeIpTables::new()?,
            ports: HashSet::default(),
            listen_port,
            write_streams: HashMap::default(),
            read_streams: StreamMap::default(),
        })
    }

    pub fn handle_client_message(&mut self, message: ClientStealTcp) -> Result<()> {
        use ClientStealTcp::*;
        match message {
            PortSubscribe(port) => self.iptables.add_redirect(port, self.listen_port),
            ConnectionUnsubscribe(connection_id) => unimplemented!(),
            PortUnsubscribe(port) => unimplemented!(),
            Data(data) => unimplemented!(),
        }
    }

    pub fn handle_incoming_connection(&mut self, stream: TcpStream, address: SocketAddr) -> Result<()> {
        let real_addr = orig_dst::orig_dst_addr(&stream)?;
        if !self.ports.contains(&real_addr.port()) {
            return Err(AgentError::UnexpectedConnection(real_addr.port()));
        }
        let (side_a, mut side_b) = duplex(1024);
        let new_connection = DaemonTcp::NewConnection(NewTcpConnection {
            stream: side_a,
            destination_port: real_addr.port(),
            address
        });
        tx.send(new_connection).await.unwrap();
        tokio::spawn(async move{
            let _ = copy_bidirectional(&mut side_b, &mut stream).await;
        });

    
        Ok(())
    }
}
async fn steal_worker(
    mut rx: Receiver<ClientStealTcp>,
    tx: Sender<DaemonTcp>,
    watch: Watch,
    pid: Option<u64>,
) -> Result<()> {
    debug!("setting namespace");
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace)?;
    }
    debug!("preparing sniffer");
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let listen_port = listener.local_addr()?.port();
    let mut ports: HashSet<Port> = HashSet::new();
    let mut worker = StealWorker::new(tx, listen_port)?;
    loop {
        select! {
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    worker.handle_client_message(msg)?;
                } else {
                    debug!("rx closed, breaking");
                    break;
                }
            },
            accept = listener.accept() => {
                match accept {
                    Ok((mut stream, address)) => {
                        worker.handle_incoming_connection(stream, address).await?;
                    },
                    Err(err) => {
                        error!("accept error {err:?}");
                        break;
                    }
                }
            }
        }
    }
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
