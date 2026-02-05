#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::sync::OnceLock;
use std::{
    assert_matches::assert_matches,
    collections::HashMap,
    fmt::{self, Debug},
    fs::File,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use actix_codec::Framed;
use futures::{SinkExt, StreamExt};
use mirrord_config::{
    LayerConfig, LayerFileConfig, MIRRORD_LAYER_INTPROXY_ADDR,
    config::{ConfigContext, MirrordConfig},
    experimental::ExperimentalFileConfig,
};
use mirrord_intproxy::{IntProxy, agent_conn::AgentConnection};
use mirrord_protocol::{
    ClientMessage, ConnectionId, DaemonCodec, DaemonMessage, FileRequest, FileResponse, ToPayload,
    file::{
        AccessFileRequest, AccessFileResponse, MetadataInternal, OpenFileRequest,
        OpenOptionsInternal, ReadFileRequest, SeekFromInternal, XstatFsResponseV2, XstatRequest,
        XstatResponse,
    },
    outgoing::{
        DaemonConnect, DaemonConnectV2, LayerConnectV2, SocketAddress,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    tcp::{DaemonTcp, LayerTcp, NewTcpConnectionV1, TcpClose, TcpData},
    uid::Uid,
};
#[cfg(target_os = "macos")]
use mirrord_sip::{SipPatchOptions, sip_patch};
pub use mirrord_tests::utils::process::TestProcess;
use rstest::fixture;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    process::Command,
};
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};

/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_PEERS: &str = "1.1.1.1:1111,2.2.2.2:2222,3.3.3.3:3333";
/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_LOCAL: &str = "4.4.4.4:4444";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static MIRRORD_MACOS_ARM64_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Initializes tracing for the current thread, allowing us to have multiple tracing subscribers
/// writin logs to different files.
///
/// We take advantage of how Rust's thread naming scheme for tests to create the log files,
/// and if we have no thread name, then we just write the logs to `stderr`.
pub fn init_tracing() -> DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("mirrord=trace"))
        .without_time()
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::ACTIVE)
        .pretty();

    // Sets the default subscriber for the _current thread_, returning a guard that unsets
    // the default subscriber when it is dropped.
    match std::thread::current()
        .name()
        .map(|name| name.replace(':', "_"))
    {
        Some(test_name) => {
            let mut logs_file = PathBuf::from("/tmp/intproxy_logs");

            #[cfg(target_os = "macos")]
            logs_file.push("macos");
            #[cfg(not(target_os = "macos"))]
            logs_file.push("linux");

            let _ = std::fs::create_dir_all(&logs_file).ok();

            logs_file.push(&test_name);
            match File::create(&logs_file) {
                // Writes the logs to the file.
                Ok(file) => {
                    println!("Created intproxy log file: {}", logs_file.display());
                    let subscriber = subscriber.with_writer(Arc::new(file)).finish();
                    tracing::subscriber::set_default(subscriber)
                }
                // File creation failure makes the output go to `stderr`.
                Err(error) => {
                    println!(
                        "Failed to create intproxy log file at {}: {error}. Intproxy logs will be flushed to stderr",
                        logs_file.display()
                    );
                    let subscriber = subscriber.with_writer(io::stderr).finish();
                    tracing::subscriber::set_default(subscriber)
                }
            }
        }
        // No thread name makes the output go to `stderr`.
        None => {
            println!(
                "Failed to obtain current thread name, intproxy logs will be flushed to stderr"
            );
            let subscriber = subscriber.with_writer(io::stderr).finish();
            tracing::subscriber::set_default(subscriber)
        }
    }
}

pub struct TestIntProxy {
    codec: Framed<TcpStream, DaemonCodec>,
    num_connections: u64,
}

impl TestIntProxy {
    pub async fn new(listener: TcpListener, config: Option<&Path>) -> Self {
        let fake_agent_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_agent_address = fake_agent_listener.local_addr().unwrap();
        let mut context = ConfigContext::default();
        let experimental_config = match config {
            Some(path) => {
                LayerFileConfig::from_path(path, &mut context)
                    .unwrap()
                    .generate_config(&mut context)
                    .unwrap()
                    .experimental
            }
            None => ExperimentalFileConfig::default()
                .generate_config(&mut context)
                .unwrap(),
        };

        tokio::spawn(async move {
            let agent_conn = AgentConnection::new_for_raw_address(fake_agent_address)
                .await
                .unwrap();
            let intproxy = IntProxy::new_with_connection(
                agent_conn,
                listener,
                0,
                Default::default(),
                Duration::from_secs(60),
                &experimental_config,
            );
            intproxy
                .run(Duration::from_secs(5), Duration::from_secs(5))
                .await
                .unwrap();
            println!("test IntProxy finished");
        });

        let (stream, _buffer_size) = fake_agent_listener.accept().await.unwrap();
        let codec = Framed::new(stream, DaemonCodec::default());

        Self {
            codec,
            num_connections: 0,
        }
    }

    pub async fn recv(&mut self) -> ClientMessage {
        self.try_recv().await.expect("intproxy connection closed")
    }

    pub async fn recv_tcp_connect(&mut self) -> (Uid, SocketAddr) {
        match self.recv().await {
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
                uid,
                remote_address: SocketAddress::Ip(addr),
            })) => {
                println!("Received TCP connect request for address {addr} with uid {uid}");
                (uid, addr)
            }
            other => panic!("unexpected message received from the intproxy: {other:?}"),
        }
    }

    pub async fn send_tcp_connect_ok(
        &mut self,
        uid: Uid,
        connection_id: ConnectionId,
        remote_addr: SocketAddr,
        local_addr: SocketAddr,
    ) {
        self.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(
            DaemonConnectV2 {
                uid,
                connect: Ok(DaemonConnect {
                    connection_id,
                    remote_address: remote_addr.into(),
                    local_address: local_addr.into(),
                }),
            },
        )))
        .await
    }

    pub async fn recv_udp_connect(&mut self) -> (Uid, SocketAddr) {
        match self.recv().await {
            ClientMessage::UdpOutgoing(LayerUdpOutgoing::ConnectV2(LayerConnectV2 {
                uid,
                remote_address: SocketAddress::Ip(addr),
            })) => {
                println!("Received UDP connect request for address {addr} with uid {uid}");
                (uid, addr)
            }
            other => panic!("unexpected message received from the intproxy: {other:?}"),
        }
    }

    pub async fn send_udp_connect_ok(
        &mut self,
        uid: Uid,
        connection_id: ConnectionId,
        remote_addr: SocketAddr,
        local_addr: SocketAddr,
    ) {
        self.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::ConnectV2(
            DaemonConnectV2 {
                uid,
                connect: Ok(DaemonConnect {
                    connection_id,
                    remote_address: remote_addr.into(),
                    local_address: local_addr.into(),
                }),
            },
        )))
        .await
    }

    pub async fn try_recv(&mut self) -> Option<ClientMessage> {
        loop {
            let msg = self.codec.next().await?.expect("inproxy connection failed");

            match msg {
                ClientMessage::Ping => {
                    self.send(DaemonMessage::Pong).await;
                }
                ClientMessage::SwitchProtocolVersion(version) => {
                    self.send(DaemonMessage::SwitchProtocolVersionResponse(version))
                        .await;
                }
                ClientMessage::ReadyForLogs => {}
                other => break Some(other),
            }
        }
    }

    pub async fn send(&mut self, msg: DaemonMessage) {
        self.codec
            .send(msg)
            .await
            .expect("intproxy connection failed");
    }

    pub async fn new_with_app_port(
        listener: TcpListener,
        app_port: u16,
        config: Option<&Path>,
    ) -> Self {
        let mut res = Self::new(listener, config).await;

        let msg = res.recv().await;
        println!("Got first message from library: {:?}", msg);

        match msg {
            ClientMessage::Tcp(LayerTcp::PortSubscribe(port)) => {
                assert_eq!(app_port, port);
                res.send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
                    .await;
                res
            }
            ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
                path,
                open_options:
                    OpenOptionsInternal {
                        read: true,
                        write: false,
                        append: false,
                        truncate: false,
                        create: false,
                        create_new: false,
                    },
            })) => {
                assert_eq!(path, PathBuf::from("/etc/hostname"));

                res.handle_gethostname::<false>(Some(app_port)).await;
                res
            }
            unexpected => panic!("Initialized connection with unexpected message {unexpected:#?}"),
        }
    }

    /// Handles the `gethostname` hook that fiddles with the agent's file system by opening
    /// `/etc/hostname` remotely.
    ///
    /// This hook leverages our ability of opening a file on the agent's `FileManager` to `open`,
    /// `read`, and `close` the `/etc/hostname` file, and thus some of the integration tests that
    /// rely on hostname resolving must handle these messages before we resume the normal flow for
    /// mirroring/stealing/outgoing traffic.
    ///
    /// ## Args
    ///
    /// - `FIRST_CALL`: some tests will consume the first message from [`Self::codec`], so we use
    ///   this `const` to check if we should call `codec.next` or if it was already called for us.
    ///   If you're using [`LayerConnection::get_initialized_connection_with_port`], you should set
    ///   this to `false`.
    pub async fn handle_gethostname<const FIRST_CALL: bool>(
        &mut self,
        app_port: Option<u16>,
    ) -> Option<()> {
        // Should we call `codec.next` or was it called outside already?
        if FIRST_CALL {
            // open file
            let open_file_request = self.recv().await;

            assert_eq!(
                open_file_request,
                ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
                    path: PathBuf::from("/etc/hostname"),
                    open_options: OpenOptionsInternal {
                        read: true,
                        write: false,
                        append: false,
                        truncate: false,
                        create: false,
                        create_new: false
                    }
                }))
            );
        }
        self.answer_file_open().await;

        // read file
        let read_request = self
            .codec
            .next()
            .await
            .expect("Read request success!")
            .expect("Read request exists!");
        assert_eq!(
            read_request,
            ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
                remote_fd: 0xb16,
                buffer_size: 256,
            }))
        );

        self.answer_file_read(b"metalbear-hostname".to_vec()).await;

        // TODO(alex): Add a wait time here, we can end up in the "Close request success" error.
        // close file (very rarely?).
        let close_request = self
            .codec
            .next()
            .await
            .expect("Close request success!")
            .expect("Close request exists!");

        println!("Should be a close file request: {close_request:#?}");
        assert_eq!(
            close_request,
            ClientMessage::FileRequest(FileRequest::Close(
                mirrord_protocol::file::CloseFileRequest { fd: 0xb16 }
            ))
        );

        let port = app_port?;
        let port_subscribe = self
            .codec
            .next()
            .await
            .expect("PortSubscribe request success!")
            .expect("PortSubscribe request exists!");
        assert_eq!(
            port_subscribe,
            ClientMessage::Tcp(LayerTcp::PortSubscribe(port))
        );

        self.codec
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
            .await
            .expect("failed to send PortSubscribe result");

        Some(())
    }

    /// Send the layer a message telling it the target got a new incoming connection.
    /// There is no such actual connection, because there is no target, but the layer should start
    /// a mirror connection with the application.
    /// Return the id of the new connection.
    pub async fn send_new_connection(&mut self, port: u16) -> u64 {
        let new_connection_id = self.num_connections;
        self.codec
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnectionV1(
                NewTcpConnectionV1 {
                    connection_id: new_connection_id,
                    remote_address: "127.0.0.1".parse().unwrap(),
                    destination_port: port,
                    source_port: "31415".parse().unwrap(),
                    local_address: "1.1.1.1".parse().unwrap(),
                },
            )))
            .await
            .unwrap();
        self.num_connections += 1;
        new_connection_id
    }

    async fn send_tcp_data(&mut self, message_data: &str, connection_id: u64) {
        self.codec
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id,
                bytes: message_data.to_payload(),
            })))
            .await
            .unwrap();
    }

    /// Send the layer a message telling it the target got a new incoming connection.
    /// There is no such actual connection, because there is no target, but the layer should start
    /// a mirror connection with the application.
    /// Return the id of the new connection.
    pub async fn send_close(&mut self, connection_id: u64) {
        self.codec
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id,
            })))
            .await
            .unwrap();
    }

    /// Tell the layer there is a new incoming connection, then send data "from that connection".
    pub async fn send_connection_then_data(&mut self, message_data: &str, port: u16) {
        let new_connection_id = self.send_new_connection(port).await;
        self.send_tcp_data(message_data, new_connection_id).await;
        self.send_close(new_connection_id).await;
    }

    /// Verify layer hooks an `open` of file `file_name` with only read flag set. Send back answer
    /// with given `fd`.
    pub async fn expect_file_open_for_reading(&mut self, file_name: &str, fd: u64) {
        self.expect_file_open_with_options(
            file_name,
            fd,
            OpenOptionsInternal {
                read: true,
                write: false,
                append: false,
                truncate: false,
                create: false,
                create_new: false,
            },
        )
        .await
    }

    /// Verify layer hooks an `open` of file `file_name` with read flag set, and any other flags.
    /// Send back answer with given `fd`.
    pub async fn expect_file_open_with_read_flag(&mut self, file_name: &str, fd: u64) {
        // Verify the app tries to open the expected file.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::Open(
                mirrord_protocol::file::OpenFileRequest {
                    path,
                    open_options: OpenOptionsInternal { read: true, .. },
                }
            )) if path.to_str().unwrap() == file_name
        );

        // Answer open.
        self.codec
            .send(DaemonMessage::File(mirrord_protocol::FileResponse::Open(
                Ok(mirrord_protocol::file::OpenFileResponse { fd }),
            )))
            .await
            .unwrap();
    }

    /// Verify layer hooks an `open` of file `file_name` with write, truncate and create flags set.
    /// Send back answer with given `fd`.
    pub async fn expect_file_open_for_writing(&mut self, file_name: &str, fd: u64) {
        self.expect_file_open_with_options(
            file_name,
            fd,
            OpenOptionsInternal {
                read: true,
                write: true,
                append: false,
                truncate: true,
                create: true,
                create_new: false,
            },
        )
        .await
    }

    /// Verify layer hooks an `open` of file `file_name` with given open options, send back answer
    /// with given `fd`.
    pub async fn expect_file_open_with_options(
        &mut self,
        file_name: &str,
        fd: u64,
        open_options: OpenOptionsInternal,
    ) {
        // Verify the app tries to open the expected file.
        assert_eq!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::Open(
                mirrord_protocol::file::OpenFileRequest {
                    path: file_name.to_string().into(),
                    open_options,
                }
            ))
        );

        // Answer open.
        self.codec
            .send(DaemonMessage::File(mirrord_protocol::FileResponse::Open(
                Ok(mirrord_protocol::file::OpenFileResponse { fd }),
            )))
            .await
            .unwrap();
    }

    /// Handles an app trying to call `libc::rename` on a path.
    ///
    /// Both `old_path` and `new_path` go through the agent.
    pub async fn expect_file_rename(&mut self, old_path: &str, new_path: &str) {
        // Verify the app tries to rename the expected file.
        assert_eq!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::Rename(
                mirrord_protocol::file::RenameRequest {
                    old_path: old_path.to_string().into(),
                    new_path: new_path.to_string().into(),
                }
            ))
        );

        // Answer rename.
        self.codec
            .send(DaemonMessage::File(mirrord_protocol::FileResponse::Rename(
                Ok(()),
            )))
            .await
            .unwrap();
    }

    /// Like the other expect_file_open_... but where we don't compare to predefined open options.
    pub async fn expect_file_open_with_whatever_options(&mut self, file_name: &str, fd: u64) {
        // Verify the app tries to open the expected file.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::Open(
                mirrord_protocol::file::OpenFileRequest {
                    path,
                    open_options: _,
                }
            )) if path.to_str().unwrap() == file_name
        );

        // Answer open.
        self.codec
            .send(DaemonMessage::File(mirrord_protocol::FileResponse::Open(
                Ok(mirrord_protocol::file::OpenFileResponse { fd }),
            )))
            .await
            .unwrap();
    }

    /// Makes a [`FileRequest::ReadLink`], and answers it.
    pub async fn expect_read_link(&mut self, file_name: &str) {
        // Expecting `readlink` call with path.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::ReadLink(
                mirrord_protocol::file::ReadLinkFileRequest { path }
            )) if path.to_str().unwrap() == file_name
        );

        // Answer `readlink`.
        self.codec
            .send(DaemonMessage::File(
                mirrord_protocol::FileResponse::ReadLink(Ok(
                    mirrord_protocol::file::ReadLinkFileResponse {
                        path: PathBuf::from_str("/gatos/rajado.txt")
                            .expect("Valid path `rajado.txt`!"),
                    },
                )),
            ))
            .await
            .unwrap();
    }

    /// Makes a [`FileRequest::MakeDir`] and answers it.
    pub async fn expect_make_dir(&mut self, expected_dir_name: &str, expected_mode: u32) {
        // Expecting `mkdir` call with path.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::MakeDir(
                mirrord_protocol::file::MakeDirRequest { pathname, mode }
            )) if pathname.to_str().unwrap() == expected_dir_name && mode == expected_mode
        );

        // Answer `mkdir`.
        self.codec
            .send(DaemonMessage::File(
                mirrord_protocol::FileResponse::MakeDir(Ok(())),
            ))
            .await
            .unwrap();
    }

    /// Makes a [`FileRequest::Statefs`] and answers it.
    pub async fn expect_statfs(&mut self, expected_path: &str) {
        // Expecting `statfs` call with path.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::StatFsV2(
                mirrord_protocol::file::StatFsRequestV2 { path }
            )) if path.to_str().unwrap() == expected_path
        );

        // Answer `statfs`.
        self.codec
            .send(DaemonMessage::File(FileResponse::XstatFsV2(Ok(
                XstatFsResponseV2 {
                    metadata: Default::default(),
                },
            ))))
            .await
            .unwrap();
    }

    /// Makes a [`FileRequest::Xstatefs`] and answers it.
    pub async fn expect_fstatfs(&mut self, expected_fd: u64) {
        // Expecting `fstatfs` call with path.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::XstatFsV2(
                mirrord_protocol::file::XstatFsRequestV2 { fd }
            )) if expected_fd == fd
        );

        // Answer `fstatfs`.
        self.codec
            .send(DaemonMessage::File(FileResponse::XstatFsV2(Ok(
                XstatFsResponseV2 {
                    metadata: Default::default(),
                },
            ))))
            .await
            .unwrap();
    }

    /// Makes a [`FileRequest::RemoveDir`] and answers it.
    pub async fn expect_remove_dir(&mut self, expected_dir_name: &str) {
        // Expecting `rmdir` call with path.
        assert_matches!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::RemoveDir(
                mirrord_protocol::file::RemoveDirRequest { pathname }
            )) if pathname.to_str().unwrap() == expected_dir_name
        );

        // Answer `rmdir`.
        self.codec
            .send(DaemonMessage::File(
                mirrord_protocol::FileResponse::RemoveDir(Ok(())),
            ))
            .await
            .unwrap();
    }

    /// Verify that the passed message (not the next message from self.codec!) is a file read.
    /// Return buffer size.
    pub async fn expect_message_file_read(message: ClientMessage, expected_fd: u64) -> u64 {
        // Verify the app reads the file.
        if let ClientMessage::FileRequest(FileRequest::Read(
            mirrord_protocol::file::ReadFileRequest {
                remote_fd: requested_fd,
                buffer_size,
            },
        )) = message
        {
            assert_eq!(expected_fd, requested_fd);
            return buffer_size;
        }
        panic!("Expected Read FileRequest. Got {message:?}");
    }

    /// Verify the layer hooks a read of `expected_fd`, return buffer size.
    pub async fn expect_only_file_read(&mut self, expected_fd: u64) -> u64 {
        // Verify the app reads the file.
        Self::expect_message_file_read(self.codec.next().await.unwrap().unwrap(), expected_fd).await
    }

    pub async fn answer_file_open(&mut self) {
        self.codec
            .send(DaemonMessage::File(FileResponse::Open(Ok(
                mirrord_protocol::file::OpenFileResponse { fd: 0xb16 },
            ))))
            .await
            .unwrap();
    }

    /// Send file read response with given `contents`.
    pub async fn answer_file_read(&mut self, contents: Vec<u8>) {
        let read_amount = contents.len();
        self.codec
            .send(DaemonMessage::File(FileResponse::Read(Ok(
                mirrord_protocol::file::ReadFileResponse {
                    bytes: contents.into(),
                    read_amount: read_amount as u64,
                },
            ))))
            .await
            .unwrap();
    }

    /// Answer an already verified file read request, then expect another one and answer with 0
    /// bytes.
    pub async fn answer_file_read_twice(
        &mut self,
        contents: &str,
        expected_fd: u64,
        buffer_size: u64,
    ) {
        let contents = contents
            .as_bytes()
            .get(0..buffer_size as usize)
            .unwrap_or(contents.as_bytes())
            .to_vec();
        self.answer_file_read(contents).await;
        // last call returns 0.
        let _buffer_size = self.expect_only_file_read(expected_fd).await;
        self.answer_file_read(vec![]).await;
    }

    /// Verify the layer hooks a read of `expected_fd`, return buffer size.
    pub async fn expect_file_read(&mut self, contents: &str, expected_fd: u64) {
        let buffer_size = self.expect_only_file_read(expected_fd).await;
        self.answer_file_read_twice(contents, expected_fd, buffer_size)
            .await
    }

    /// Verify the layer hooks a read of `expected_fd`, return buffer size.
    pub async fn consume_xstats_then_expect_file_read(&mut self, contents: &str, expected_fd: u64) {
        let message = self.consume_xstats().await;
        let buffer_size = Self::expect_message_file_read(message, expected_fd).await;
        self.answer_file_read_twice(contents, expected_fd, buffer_size)
            .await
    }

    /// For when the application does not keep reading until it gets 0 bytes.
    pub async fn expect_single_file_read(&mut self, contents: &str, expected_fd: u64) {
        let buffer_size = self.expect_only_file_read(expected_fd).await;
        let contents = contents
            .as_bytes()
            .get(0..buffer_size as usize)
            .unwrap_or(contents.as_bytes())
            .to_vec();
        self.answer_file_read(contents).await;
    }

    /// Verify that the layer sends a file write request with the expected contents.
    /// Send back response.
    pub async fn consume_xstats_then_expect_file_write(&mut self, contents: &str, fd: u64) {
        let message = self.consume_xstats().await;
        assert_eq!(
            message,
            ClientMessage::FileRequest(FileRequest::Write(
                mirrord_protocol::file::WriteFileRequest {
                    fd,
                    write_bytes: contents.to_payload()
                }
            ))
        );

        let written_amount = contents.len() as u64;
        self.codec
            .send(DaemonMessage::File(FileResponse::Write(Ok(
                mirrord_protocol::file::WriteFileResponse { written_amount },
            ))))
            .await
            .unwrap();
    }

    /// Verify that the layer sends a file write request with the expected contents.
    /// Send back response.
    pub async fn consume_xstats_then_expect_file_lseek(
        &mut self,
        seek_from: SeekFromInternal,
        fd: u64,
    ) {
        let message = self.consume_xstats().await;
        assert_eq!(
            message,
            ClientMessage::FileRequest(FileRequest::Seek(
                mirrord_protocol::file::SeekFileRequest { fd, seek_from }
            ))
        );

        self.codec
            .send(DaemonMessage::File(FileResponse::Seek(Ok(
                mirrord_protocol::file::SeekFileResponse { result_offset: 0 },
            ))))
            .await
            .unwrap();
    }

    /// Read next layer message and verify it's a close request.
    pub async fn expect_file_close(&mut self, fd: u64) {
        assert_eq!(
            self.codec.next().await.unwrap().unwrap(),
            ClientMessage::FileRequest(FileRequest::Close(
                mirrord_protocol::file::CloseFileRequest { fd }
            ))
        );
    }

    /// Verify the next message from the layer is an access to the given path with the given mode.
    /// Send back a response.
    pub async fn expect_file_access(&mut self, pathname: PathBuf, mode: u8) {
        assert_eq!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::Access(AccessFileRequest { pathname, mode }))
        );

        self.codec
            .send(DaemonMessage::File(FileResponse::Access(Ok(
                AccessFileResponse {},
            ))))
            .await
            .unwrap();
    }

    /// Assert that the layer sends an xstat request with the given fd, answer the request with the
    /// given metadata.
    pub async fn expect_xstat_with_metadata(
        &mut self,
        path: Option<PathBuf>,
        fd: Option<u64>,
        metadata: MetadataInternal,
    ) {
        assert_eq!(
            self.recv().await,
            ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
                path,
                fd,
                follow_symlink: true,
            }))
        );

        self.codec
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await
            .unwrap();
    }

    /// Assert that the layer sends an xstat request with the given fd, answer the request.
    pub async fn expect_xstat(&mut self, path: Option<PathBuf>, fd: Option<u64>) {
        self.expect_xstat_with_metadata(path, fd, Default::default())
            .await
    }

    /// Consume messages from the codec and return the first non-xstat message.
    pub async fn consume_xstats(&mut self) -> ClientMessage {
        let mut message = self.recv().await;
        while let ClientMessage::FileRequest(FileRequest::Xstat(_xstat_request)) = message {
            // Answer xstat.
            self.codec
                .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                    XstatResponse {
                        metadata: Default::default(),
                    },
                ))))
                .await
                .unwrap();
            message = self.recv().await;
        }
        message
    }

    /// Expect all the requested requests for gethostname hook
    pub async fn expect_gethostname(&mut self, fd: u64) {
        self.expect_file_open_for_reading("/etc/hostname", fd).await;

        self.expect_single_file_read("foobar\n", fd).await;

        self.expect_file_close(fd).await;
    }
}

/// Go versions used with test applications.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy)]
pub enum GoVersion {
    GO_1_23,
    GO_1_24,
    GO_1_25,
}

impl fmt::Display for GoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::GO_1_23 => "23",
            Self::GO_1_24 => "24",
            Self::GO_1_25 => "25",
        };
        f.write_str(as_str)
    }
}

/// Various applications used by integration tests.
#[derive(Debug)]
pub enum Application {
    GoHTTP(GoVersion),
    NodeHTTP,
    PythonFastApiHTTP,
    /// Shared sockets [#864](https://github.com/metalbear-co/mirrord/issues/864).
    PythonIssue864,
    PythonFlaskHTTP,
    PythonSelfConnect,
    PythonDontLoad,
    PythonListen,
    RustFileOps,
    GoFileOps(GoVersion),
    JavaTemurinSip,
    EnvBashCat,
    NodeFileOps,
    NodeSpawn,
    NodeCopyFile,
    NodeIssue2903,
    GoDir(GoVersion),
    GoDirBypass(GoVersion),
    GoIssue834(GoVersion),
    BashShebang,
    GoRead(GoVersion),
    GoWrite(GoVersion),
    GoLSeek(GoVersion),
    GoFAccessAt(GoVersion),
    GoSelfOpen(GoVersion),
    RustOutgoingUdp,
    RustOutgoingTcp {
        non_blocking: bool,
    },
    RustIssue1123,
    RustIssue1054,
    RustIssue1458,
    RustIssue1458PortNot53,
    RustIssue1776,
    RustIssue1776PortNot53,
    RustIssue1899,
    RustIssue2001,
    RustDnsResolve,
    RustRecvFrom,
    RustListenPorts,
    Fork,
    ReadLink,
    StatfsFstatfs,
    MkdirRmdir,
    OpenFile,
    CIssue2055,
    CIssue2178,
    RustIssue2058,
    Realpath,
    NodeIssue2283,
    RustIssue2204,
    RustIssue2438,
    RustIssue3248,
    NodeIssue2807,
    RustRebind0,
    /// Go application that simply opens a file.
    GoOpen {
        /// Path to the file, accepted as `-p` param.
        path: String,
        /// Flags to use when opening the file, accepted as `-f` param.
        flags: i32,
        /// Mode to use when opening the file, accepted as `-m` param.
        mode: u32,
        version: GoVersion,
    },
    /// For running applications with the executable and arguments determined at runtime.
    DynamicApp(String, Vec<String>),
    /// Go app that only checks whether Linux pidfd syscalls are supported.
    GoIssue2988(GoVersion),
    NodeMakeConnections,
    NodeIssue3456,
    /// C++ app that dlopen c-shared go library.
    DlopenCgo,
    /// C app that calls BSD connectx(2).
    Connectx,
    Dup,
}

impl Application {
    /// Run python with shell resolving to find the actual executable.
    ///
    /// This is to help tests that run python with mirrord work locally on systems with pyenv.
    /// If we run `python3` on a system with pyenv the first executed is not python but bash. On mac
    /// that prevents the layer from loading because of SIP.
    async fn get_python3_executable() -> String {
        let mut python = Command::new("python3")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let child_stdin = python.stdin.as_mut().unwrap();
        child_stdin
            .write_all(b"import sys\nprint(sys.executable)")
            .await
            .unwrap();
        let output = python.wait_with_output().await.unwrap();
        String::from(String::from_utf8_lossy(&output.stdout).trim())
    }

    pub async fn get_executable(&self) -> String {
        match self {
            Application::PythonFlaskHTTP
            | Application::PythonSelfConnect
            | Application::PythonDontLoad
            | Application::PythonListen => Self::get_python3_executable().await,
            Application::PythonFastApiHTTP | Application::PythonIssue864 => String::from("uvicorn"),
            Application::Fork => String::from("tests/apps/fork/out.c_test_app"),
            Application::ReadLink => String::from("tests/apps/readlink/out.c_test_app"),
            Application::StatfsFstatfs => String::from("tests/apps/statfs_fstatfs/out.c_test_app"),
            Application::MkdirRmdir => String::from("tests/apps/mkdir_rmdir/out.c_test_app"),
            Application::Realpath => String::from("tests/apps/realpath/out.c_test_app"),
            Application::NodeHTTP
            | Application::NodeIssue2283
            | Application::NodeIssue2807
            | Application::NodeIssue3456 => String::from("node"),
            Application::JavaTemurinSip => format!(
                "{}/.sdkman/candidates/java/17.0.6-tem/bin/java",
                std::env::var("HOME").unwrap(),
            ),
            Application::GoHTTP(version) => format!("tests/apps/app_go/{version}.go_test_app"),
            Application::GoFileOps(version) => {
                format!("tests/apps/fileops/go/{version}.go_test_app")
            }
            Application::RustFileOps => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/fileops"
                )
            }
            Application::EnvBashCat => String::from("tests/apps/env_bash_cat.sh"),
            Application::NodeFileOps
            | Application::NodeSpawn
            | Application::NodeCopyFile
            | Application::NodeIssue2903
            | Application::NodeMakeConnections => String::from("node"),
            Application::GoDir(version) => format!("tests/apps/dir_go/{version}.go_test_app"),
            Application::GoIssue834(version) => {
                format!("tests/apps/issue834/{version}.go_test_app")
            }
            Application::GoDirBypass(version) => {
                format!("tests/apps/dir_go_bypass/{version}.go_test_app")
            }
            Application::BashShebang => String::from("tests/apps/nothing.sh"),
            Application::GoRead(version) => format!("tests/apps/read_go/{version}.go_test_app"),
            Application::GoWrite(version) => format!("tests/apps/write_go/{version}.go_test_app"),
            Application::GoLSeek(version) => format!("tests/apps/lseek_go/{version}.go_test_app"),
            Application::GoFAccessAt(version) => {
                format!("tests/apps/faccessat_go/{version}.go_test_app")
            }
            Application::GoSelfOpen(version) => {
                format!("tests/apps/self_open/{version}.go_test_app")
            }
            Application::RustIssue1123 => String::from("tests/apps/issue1123/target/issue1123"),
            Application::RustIssue1054 => String::from("tests/apps/issue1054/target/issue1054"),
            Application::RustIssue1458 => String::from("tests/apps/issue1458/target/issue1458"),
            Application::RustIssue1458PortNot53 => {
                String::from("tests/apps/issue1458portnot53/target/issue1458portnot53")
            }
            Application::RustOutgoingUdp | Application::RustOutgoingTcp { .. } => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "../../target/debug/outgoing",
            ),
            Application::RustDnsResolve => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "../../target/debug/dns_resolve",
            ),
            Application::RustRecvFrom => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/recv_from"
                )
            }
            Application::RustListenPorts => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/listen_ports"
                )
            }
            Application::RustIssue1776 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue1776"
                )
            }
            Application::RustIssue1776PortNot53 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue1776portnot53"
                )
            }
            Application::RustIssue1899 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue1899"
                )
            }
            Application::RustIssue2438 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue2438"
                )
            }
            Application::RustIssue2001 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue2001"
                )
            }
            Application::RustIssue3248 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue3248"
                )
            }
            Application::RustRebind0 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/rebind0"
                )
            }
            Application::OpenFile => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "tests/apps/open_file/out.c_test_app",
            ),
            Application::CIssue2055 => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "tests/apps/gethostbyname/out.c_test_app",
            ),
            Application::CIssue2178 => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "tests/apps/issue2178/out.c_test_app",
            ),
            Application::RustIssue2058 => String::from("tests/apps/issue2058/target/issue2058"),
            Application::RustIssue2204 => String::from("tests/apps/issue2204/target/issue2204"),
            Application::GoOpen { version, .. } => {
                format!("tests/apps/open_go/{version}.go_test_app")
            }
            Application::DynamicApp(exe, _) => exe.clone(),
            Application::GoIssue2988(version) => {
                format!("tests/apps/issue2988/{version}.go_test_app")
            }
            Application::DlopenCgo => String::from("tests/apps/dlopen_cgo/out.cpp_dlopen_cgo"),
            Application::Connectx => String::from("tests/apps/connectx/out.c_test_app"),
            Application::Dup => String::from("tests/apps/dup/out.c_test_app"),
        }
    }

    pub fn get_args(&self) -> Vec<String> {
        let mut app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        app_path.push("tests/apps/");
        match self {
            Application::JavaTemurinSip => {
                app_path.push("java_temurin_sip/src/Main.java");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::PythonFlaskHTTP => {
                app_path.push("app_flask.py");
                println!("using flask server from {app_path:?}");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::PythonDontLoad => {
                app_path.push("dont_load.py");
                println!("using script from {app_path:?}");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::PythonListen => {
                app_path.push("app_listen.py");
                println!("using script from {app_path:?}");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::PythonFastApiHTTP => vec![
                String::from("--port=9999"),
                String::from("--host=0.0.0.0"),
                String::from("--app-dir=tests/apps/"),
                String::from("app_fastapi:app"),
            ],
            Application::PythonIssue864 => {
                vec![
                    String::from("--reload"),
                    String::from("--port=9999"),
                    String::from("--host=0.0.0.0"),
                    String::from("--app-dir=tests/apps/"),
                    String::from("shared_sockets:app"),
                ]
            }
            Application::NodeHTTP => {
                app_path.push("app_node.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeFileOps => {
                app_path.push("fileops.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue3456 => {
                app_path.push("issue3456.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeSpawn => {
                app_path.push("node_spawn.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeCopyFile => {
                app_path.push("node_copyfile.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2903 => {
                app_path.push("issue2903.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2283 => {
                app_path.push("issue2883.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2807 => {
                app_path.push("issue2807.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeMakeConnections => {
                app_path.push("make_connections.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::PythonSelfConnect => {
                app_path.push("self_connect.py");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::GoHTTP(..)
            | Application::GoDir(..)
            | Application::GoFileOps(..)
            | Application::GoIssue834(..)
            | Application::GoRead(..)
            | Application::GoWrite(..)
            | Application::GoLSeek(..)
            | Application::GoFAccessAt(..)
            | Application::Fork
            | Application::ReadLink
            | Application::StatfsFstatfs
            | Application::MkdirRmdir
            | Application::Realpath
            | Application::RustFileOps
            | Application::RustIssue1123
            | Application::RustIssue1054
            | Application::RustIssue1458
            | Application::RustIssue1458PortNot53
            | Application::RustIssue1776
            | Application::RustIssue1776PortNot53
            | Application::RustIssue1899
            | Application::RustIssue2001
            | Application::RustDnsResolve
            | Application::RustRecvFrom
            | Application::RustListenPorts
            | Application::EnvBashCat
            | Application::BashShebang
            | Application::GoSelfOpen(..)
            | Application::GoDirBypass(..)
            | Application::RustIssue2058
            | Application::OpenFile
            | Application::CIssue2055
            | Application::CIssue2178
            | Application::RustIssue2204
            | Application::RustRebind0
            | Application::RustIssue2438
            | Application::RustIssue3248
            | Application::GoIssue2988(..)
            | Application::DlopenCgo
            | Application::Connectx
            | Application::Dup => vec![],
            Application::RustOutgoingUdp => ["--udp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::RustOutgoingTcp {
                non_blocking: false,
            } => ["--tcp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::RustOutgoingTcp { non_blocking: true } => [
                "--tcp",
                RUST_OUTGOING_LOCAL,
                RUST_OUTGOING_PEERS,
                "--non-blocking",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            Application::GoOpen {
                path, flags, mode, ..
            } => {
                vec![
                    "-p".to_string(),
                    path.clone(),
                    "-f".to_string(),
                    flags.to_string(),
                    "-m".to_string(),
                    mode.to_string(),
                ]
            }
            Application::DynamicApp(_, args) => args.to_owned(),
        }
    }

    pub fn get_app_port(&self) -> u16 {
        match self {
            Application::GoHTTP(..)
            | Application::GoFileOps(..)
            | Application::NodeHTTP
            | Application::RustIssue1054
            | Application::PythonFlaskHTTP => 80,
            // mapped from 9999 in `configs/port_mapping.json`
            Application::PythonFastApiHTTP | Application::PythonIssue864 => 1234,
            Application::RustIssue1123 => 41222,
            Application::PythonListen => 21232,
            Application::PythonDontLoad
            | Application::RustFileOps
            | Application::RustDnsResolve
            | Application::JavaTemurinSip
            | Application::EnvBashCat
            | Application::NodeFileOps
            | Application::NodeSpawn
            | Application::NodeCopyFile
            | Application::NodeIssue2903
            | Application::NodeIssue3456
            | Application::BashShebang
            | Application::Fork
            | Application::ReadLink
            | Application::StatfsFstatfs
            | Application::MkdirRmdir
            | Application::Realpath
            | Application::GoIssue834(..)
            | Application::GoRead(..)
            | Application::GoWrite(..)
            | Application::GoLSeek(..)
            | Application::GoFAccessAt(..)
            | Application::GoDirBypass(..)
            | Application::GoSelfOpen(..)
            | Application::GoDir(..)
            | Application::RustOutgoingUdp
            | Application::RustOutgoingTcp { .. }
            | Application::RustIssue1458
            | Application::RustIssue1458PortNot53
            | Application::RustIssue1776
            | Application::RustIssue1776PortNot53
            | Application::RustIssue1899
            | Application::RustIssue2001
            | Application::RustListenPorts
            | Application::RustRecvFrom
            | Application::OpenFile
            | Application::CIssue2055
            | Application::CIssue2178
            | Application::NodeIssue2283
            | Application::RustIssue2204
            | Application::RustIssue2438
            | Application::RustIssue3248
            | Application::NodeIssue2807
            | Application::RustRebind0
            | Application::GoOpen { .. }
            | Application::DynamicApp(..)
            | Application::GoIssue2988(..)
            | Application::NodeMakeConnections
            | Application::Connectx => unimplemented!("shouldn't get here"),
            Application::PythonSelfConnect => 1337,
            Application::RustIssue2058 => 1234,
            Application::DlopenCgo => 23333,
            Application::Dup => 42069,
        }
    }

    /// Start the test process with the given env.
    pub async fn get_test_process(&self, env: HashMap<String, String>) -> TestProcess {
        let executable = self.get_executable().await;
        #[cfg(target_os = "macos")]
        let executable = sip_patch(&executable, SipPatchOptions::default(), None)
            .unwrap()
            .unwrap_or(executable);
        println!("Using executable: {}", &executable);
        println!("Using args: {:?}", self.get_args());
        TestProcess::start_process(executable, self.get_args(), env).await
    }

    /// Start the process of this application, with the layer lib loaded.
    /// Will start it with env from `get_env` plus whatever is passed in `extra_env_vars`.
    pub async fn start_process_with_layer(
        &self,
        dylib_path: &Path,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = get_env(dylib_path, address, extra_env_vars, configuration_file);
        let test_process = self.get_test_process(env).await;

        (
            test_process,
            TestIntProxy::new(listener, configuration_file).await,
        )
    }

    /// Like `start_process_with_layer`, but also verify a port subscribe.
    pub async fn start_process_with_layer_and_port(
        &self,
        dylib_path: &Path,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = get_env(dylib_path, address, extra_env_vars, configuration_file);
        let test_process = self.get_test_process(env).await;

        (
            test_process,
            TestIntProxy::new_with_app_port(listener, self.get_app_port(), configuration_file)
                .await,
        )
    }
}

/// Return the path to the existing layer lib, or build it first and return the path, according to
/// whether the environment variable MIRRORD_TEST_USE_EXISTING_LIB is set.
/// When testing locally the lib should be rebuilt on each run so that when developers make changes
/// they don't have to also manually build the lib before running the tests.
/// Building is slow on the CI though, so the CI can set the env var and use an artifact of an
/// earlier job on the same run (there are no code changes in between).
#[fixture]
#[once]
pub fn dylib_path() -> PathBuf {
    match std::env::var("MIRRORD_TEST_USE_EXISTING_LIB") {
        Ok(path) => {
            let dylib_path = PathBuf::from(path);
            println!("Using existing layer lib from: {dylib_path:?}");
            assert!(dylib_path.exists());
            dylib_path
        }
        Err(_) => {
            let dylib_path = test_cdylib::build_current_project();
            println!("Built library at {dylib_path:?}");
            dylib_path
        }
    }
}

/// Fixture to get configuration files directory.
#[fixture]
#[once]
pub fn config_dir() -> PathBuf {
    let mut config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    config_path.push("tests/configs");
    config_path
}

/// Environment for the user application.
///
/// The environment includes:
/// 1. `RUST_LOG=warn,mirrord=trace`
/// 2. [`MIRRORD_LAYER_INTPROXY_ADDR`]
/// 3. Layer injection variable
/// 4. [`LayerConfig::RESOLVED_CONFIG_ENV`]
/// 6. Given `extra_vars`
///
/// `extra_vars` are also added to the [`ConfigContext`] for [`LayerConfig::resolve`].
pub fn get_env(
    dylib_path: &Path,
    intproxy_addr: SocketAddr,
    extra_vars: Vec<(&str, &str)>,
    config_path: Option<&Path>,
) -> HashMap<String, String> {
    let extra_vars_owned = extra_vars
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();

    let mut cfg_context = ConfigContext::default()
        .override_env("MIRRORD_IMPERSONATED_TARGET", "pod/mock-target")
        .override_env("MIRRORD_REMOTE_DNS", "false")
        .override_envs(extra_vars)
        .override_env_opt(LayerConfig::FILE_PATH_ENV, config_path)
        .strict_env(true);
    let config = LayerConfig::resolve(&mut cfg_context).unwrap();

    [
        ("RUST_LOG".to_string(), "warn,mirrord=debug".to_string()),
        (
            MIRRORD_LAYER_INTPROXY_ADDR.to_string(),
            intproxy_addr.to_string(),
        ),
        (
            LayerConfig::RESOLVED_CONFIG_ENV.to_string(),
            config.encode().unwrap(),
        ),
        #[cfg(target_os = "macos")]
        (
            "DYLD_INSERT_LIBRARIES".to_string(),
            dylib_path.to_str().unwrap().to_string(),
        ),
        #[cfg(target_os = "linux")]
        (
            "LD_PRELOAD".to_string(),
            dylib_path.to_str().unwrap().to_string(),
        ),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        (
            // The universal layer library loads arm64 library from the path specified in this
            // environment variable when the host runs arm64e.
            // See `mirorrd/layer/shim.c` for more information.
            "MIRRORD_MACOS_ARM64_LIBRARY".to_string(),
            arm64_dylib_path().to_str().unwrap().to_string(),
        ),
    ]
    .into_iter()
    .chain(extra_vars_owned)
    .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_dylib_path() -> &'static Path {
    MIRRORD_MACOS_ARM64_LIBRARY
        .get_or_init(|| {
            if let Ok(path) = std::env::var("MIRRORD_MACOS_ARM64_LIBRARY") {
                let dylib_path = PathBuf::from(path);
                println!("Using existing macOS arm64 layer lib from: {dylib_path:?}");
                assert!(dylib_path.exists());
                return dylib_path;
            }

            if let Ok(path) = std::env::var("MIRRORD_TEST_USE_EXISTING_LIB") {
                let derived_path = path.replace("universal-apple-darwin", "aarch64-apple-darwin");
                let dylib_path = PathBuf::from(&derived_path);
                if dylib_path.exists() {
                    return dylib_path;
                } else {
                    println!("Derived arm64 layer lib path does not exist: {derived_path}");
                }
            }

            let dylib_path = test_cdylib::build_current_project();
            println!("Built macOS arm64 layer lib at {dylib_path:?}");
            dylib_path
        })
        .as_path()
}
