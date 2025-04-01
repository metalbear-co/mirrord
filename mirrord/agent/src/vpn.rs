//! This code is not used anywhere.

#![allow(dead_code)]
use std::{
    fmt,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use mirrord_protocol::vpn::{ClientVpn, NetworkConfiguration, ServerVpn};
use nix::sys::socket::SockaddrStorage;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{
    io::unix::{AsyncFd, AsyncFdReadyGuard},
    net::UdpSocket,
    select,
    sync::mpsc::{self, Receiver, Sender},
};

use crate::{
    error::AgentResult,
    util::remote_runtime::{BgTaskStatus, IntoStatus, MaybeRemoteRuntime},
};

/// An interface for a background task handling [`ClientVpn`] messages.
/// Each agent client has their own independent instance (neither this wrapper nor the background
/// task are shared).
pub(crate) struct VpnApi {
    task_status: BgTaskStatus,
    /// Sends the layer messages to the [`VpnTask`].
    layer_tx: Sender<ClientVpn>,

    /// Reads the daemon messages from the [`VpnTask`].
    daemon_rx: Receiver<ServerVpn>,
}

impl VpnApi {
    /// Spawns a new background task for handling the `vpn` feature and creates a new instance of
    /// this struct to serve as an interface.
    ///
    /// # Params
    ///
    /// * `runtime` - tokio runtime to spawn the task on.
    pub(crate) fn new(runtime: &MaybeRemoteRuntime) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);
        let pid = runtime.target_pid();

        let task_status = runtime
            .spawn(VpnTask::new(pid, layer_rx, daemon_tx).run())
            .into_status("VpnTask");

        Self {
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    /// Sends the [`ClientVpn`] message to the background task.
    pub(crate) async fn layer_message(&mut self, message: ClientVpn) -> AgentResult<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }

    /// Receives a [`ServerVpn`] message from the background task.
    pub(crate) async fn daemon_message(&mut self) -> AgentResult<ServerVpn> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.wait_assert_running().await),
        }
    }
}

pub struct AsyncRawSocket {
    inner: AsyncFd<Socket>,
    addr: SockAddr,
}

impl AsyncRawSocket {
    pub fn new(socket: Socket, addr: SockAddr) -> std::io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self {
            inner: AsyncFd::new(socket)?,
            addr,
        })
    }

    pub async fn readable(&self) -> std::io::Result<AsyncFdReadyGuard<Socket>> {
        self.inner.readable().await
    }

    pub async fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let mut guard = self.inner.writable().await?;
            match guard.try_io(|inner| inner.get_ref().send_to(buf, &self.addr)) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }
}

async fn create_raw_socket() -> io::Result<AsyncRawSocket> {
    let index = nix::net::if_::if_nametoindex("eth0")?;

    let socket = Socket::new(
        Domain::PACKET,
        Type::DGRAM,
        Some(Protocol::from(libc::ETH_P_IP.to_be())),
    )?;
    let sock_addr = interface_index_to_sock_addr(i32::try_from(index).map_err(|_| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("invalid interface index {index}"),
        )
    })?)?;
    socket.bind(&sock_addr)?;
    socket.set_nonblocking(true)?;
    AsyncRawSocket::new(socket, sock_addr)
}

#[tracing::instrument(level = "debug", ret)]
async fn resolve_interface() -> io::Result<(IpAddr, IpAddr, IpAddr)> {
    // Connect to a remote address so we can later get the default network interface.
    let temporary_socket = UdpSocket::bind("0.0.0.0:0").await?;
    temporary_socket.connect("8.8.8.8:53").await?;

    // trigger data sent to have gateway in ARP cache.
    let _ = temporary_socket.send(&[0]).await;
    // Create comparison address here with `port: 0`, to match the network interface's address of
    // `sin_port: 0`.
    let local_address = SocketAddr::new(temporary_socket.local_addr()?.ip(), 0);
    let raw_local_address = SockaddrStorage::from(local_address);

    // Try to find an interface that matches the local ip we have.
    let usable_interface = nix::ifaddrs::getifaddrs()?
        .find(|iface| {
            iface
                .address
                .map(|addr| addr == raw_local_address)
                .unwrap_or(false)
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no usable interface"))?;

    let ip = usable_interface
        .address
        .as_ref()
        .and_then(SockaddrStorage::as_sockaddr_in)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "usable_interface.address.as_sockaddr_in",
            )
        })?
        .ip()
        .into();
    let net_mask = usable_interface
        .netmask
        .as_ref()
        .and_then(SockaddrStorage::as_sockaddr_in)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "usable_interface.netmask.as_sockaddr_in",
            )
        })?
        .ip()
        .into();
    // extracting gateway is more difficult, ugly patch for now.
    let temp_gateway = usable_interface
        .address
        .as_ref()
        .and_then(SockaddrStorage::as_sockaddr_in)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "usable_interface.address.as_sockaddr_in",
            )
        })?
        .ip()
        .octets();

    let gateway = IpAddr::V4(Ipv4Addr::new(
        temp_gateway[0],
        temp_gateway[1],
        temp_gateway[2],
        1,
    ));

    Ok((ip, net_mask, gateway))
}

/// Handles outgoing connections for one client (layer).
struct VpnTask {
    pid: Option<u64>,
    layer_rx: Receiver<ClientVpn>,
    daemon_tx: Sender<ServerVpn>,
    socket: Option<AsyncRawSocket>,
}

impl fmt::Debug for VpnTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VpnTask").field("pid", &self.pid).finish()
    }
}

fn interface_index_to_sock_addr(index: i32) -> io::Result<SockAddr> {
    let mut addr_storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let len = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
    let macs = procfs::net::arp().map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
    tracing::debug!(?macs, "arp entries");

    let hw_addr = macs
        .into_iter()
        .find_map(|entry| entry.hw_address)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no entry with hw address"))?;

    unsafe {
        let sock_addr = std::ptr::addr_of_mut!(addr_storage) as *mut libc::sockaddr_ll;
        (*sock_addr).sll_family = libc::AF_PACKET as u16;
        (*sock_addr).sll_protocol = (libc::ETH_P_IP as u16).to_be();
        (*sock_addr).sll_ifindex = index;
        (*sock_addr).sll_halen = 6;
        (*sock_addr).sll_addr = [
            hw_addr[0], hw_addr[1], hw_addr[2], hw_addr[3], hw_addr[4], hw_addr[5], 0, 0,
        ];
    }

    Ok(unsafe { SockAddr::new(addr_storage, len) })
}

impl VpnTask {
    fn new(pid: Option<u64>, layer_rx: Receiver<ClientVpn>, daemon_tx: Sender<ServerVpn>) -> Self {
        Self {
            pid,
            layer_rx,
            daemon_tx,
            socket: None,
        }
    }

    #[allow(clippy::indexing_slicing)]
    async fn run(mut self) -> io::Result<()> {
        // so host won't respond with RST to our packets.
        // TODO: need to do it for UDP as well to avoid ICMP unreachable.
        let output = std::process::Command::new("iptables")
            .args([
                "-A",
                "OUTPUT",
                "-p",
                "tcp",
                "--tcp-flags",
                "RST",
                "RST",
                "-j",
                "DROP",
            ])
            .output()?;

        tracing::debug!(?output, "iptables output");
        let (ip, net_mask, gateway) = resolve_interface().await?;
        let network_configuration = NetworkConfiguration {
            ip,
            net_mask,
            gateway,
        };

        tracing::debug!(socket_init = %self.socket.is_some(), "begin loop");

        let mut buffer = [0u8; 1024 * 64];
        loop {
            select! {
                biased;

                message = self.layer_rx.recv() => match message {
                    // We have a message from the layer to be handled.
                    Some(message) => self.handle_layer_msg(message, &network_configuration).await?,
                    // Our channel with the layer is closed, this task is no longer needed.
                    None => {
                        tracing::trace!("VpnTask -> Channel with the layer is closed, exiting.");
                        break Ok(());
                    },
                },

                // We have data coming from one of our peers.
                ready = (async { self.socket.as_mut().expect("is checked").readable().await }), if self.socket.is_some() => {
                    let mut guard = ready?;
                    match guard.try_io(|inner| inner.get_ref().read(&mut buffer)) {
                        Ok(Ok(len)) => {
                            if len > 0 {
                                let packet = buffer[..len].to_vec();
                                self.daemon_tx
                                    .send(ServerVpn::Packet(packet))
                                    .await
                                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

                                buffer[..len].fill(0);
                            }
                        },
                        Ok(Err(error)) => {
                            tracing::error!(%error, "could not read");
                        }
                        Err(_would_block) => continue,
                    }
                }
            }
        }
    }

    #[tracing::instrument(level = "trace", ret, err(Debug))]
    async fn handle_layer_msg(
        &mut self,
        message: ClientVpn,
        network_configuration: &NetworkConfiguration,
    ) -> io::Result<()> {
        match message {
            // We make connection to the requested address, split the stream into halves with
            // `io::split`, and put them into respective maps.
            ClientVpn::GetNetworkConfiguration => {
                self.daemon_tx
                    .send(ServerVpn::NetworkConfiguration(
                        network_configuration.clone(),
                    ))
                    .await
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
            }
            ClientVpn::Packet(packet) => {
                if let Some(socket) = self.socket.as_mut() {
                    socket.write(&packet).await?;
                } else {
                    tracing::error!(?packet, "unable to send packet");
                }
            }
            ClientVpn::OpenSocket => {
                self.socket.replace(create_raw_socket().await?);
            }
        }

        Ok(())
    }
}
