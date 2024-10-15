use std::{
    assert_matches::assert_matches,
    collections::HashMap,
    fmt::Debug,
    fs::File,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use actix_codec::Framed;
use futures::{SinkExt, StreamExt};
use mirrord_intproxy::{agent_conn::AgentConnection, IntProxy};
use mirrord_protocol::{
    file::{
        AccessFileRequest, AccessFileResponse, OpenFileRequest, OpenOptionsInternal,
        ReadFileRequest, SeekFromInternal, XstatRequest, XstatResponse,
    },
    tcp::{DaemonTcp, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, DaemonCodec, DaemonMessage, FileRequest, FileResponse,
};
#[cfg(target_os = "macos")]
use mirrord_sip::sip_patch;
use rstest::fixture;
pub use tests::utils::TestProcess;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    process::Command,
};
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_PEERS: &str = "1.1.1.1:1111,2.2.2.2:2222,3.3.3.3:3333";
/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_LOCAL: &str = "4.4.4.4:4444";

/// Initializes tracing for the current thread, allowing us to have multiple tracing subscribers
/// writin logs to different files.
///
/// We take advantage of how Rust's thread naming scheme for tests to create the log files,
/// and if we have no thread name, then we just write the logs to `stderr`.
pub fn init_tracing() -> Result<DefaultGuard, Box<dyn std::error::Error>> {
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
            let mut logs_file = PathBuf::from_str("/tmp/intproxy_logs")?;

            #[cfg(target_os = "macos")]
            logs_file.push("macos");
            #[cfg(not(target_os = "macos"))]
            logs_file.push("linux");

            let _ = std::fs::create_dir_all(&logs_file).ok();

            logs_file.push(&test_name);
            match File::create(logs_file) {
                Ok(file) => {
                    let subscriber = subscriber.with_writer(Arc::new(file)).finish();

                    // Writes the logs to a file.
                    Ok(tracing::subscriber::set_default(subscriber))
                }
                Err(_) => {
                    let subscriber = subscriber.with_writer(io::stderr).finish();

                    // File creation failure makes the output go to `stderr`.
                    Ok(tracing::subscriber::set_default(subscriber))
                }
            }
        }
        None => {
            let subscriber = subscriber.with_writer(io::stderr).finish();

            // No thread name makes the output go to `stderr`.
            Ok(tracing::subscriber::set_default(subscriber))
        }
    }
}

pub struct TestIntProxy {
    codec: Framed<TcpStream, DaemonCodec>,
    num_connections: u64,
}

impl TestIntProxy {
    pub async fn new(listener: TcpListener) -> Self {
        let fake_agent_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_agent_address = fake_agent_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let agent_conn = AgentConnection::new_for_raw_address(fake_agent_address)
                .await
                .unwrap();
            let intproxy = IntProxy::new_with_connection(agent_conn, listener);
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

    pub async fn new_with_app_port(listener: TcpListener, app_port: u16) -> Self {
        let mut res = Self::new(listener).await;

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
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnection(
                NewTcpConnection {
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
                bytes: Vec::from(message_data),
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
                    bytes: contents,
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
                    write_bytes: contents.as_bytes().to_vec()
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

    /// Assert that the layer sends an xstat request with the given fd, answer the request.
    pub async fn expect_xstat(&mut self, path: Option<PathBuf>, fd: Option<u64>) {
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
                XstatResponse {
                    metadata: Default::default(),
                },
            ))))
            .await
            .unwrap();
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

/// Various applications used by integration tests.
#[derive(Debug)]
pub enum Application {
    Go21HTTP,
    Go22HTTP,
    Go23HTTP,
    NodeHTTP,
    PythonFastApiHTTP,
    /// Shared sockets [#864](https://github.com/metalbear-co/mirrord/issues/864).
    PythonIssue864,
    PythonFlaskHTTP,
    PythonSelfConnect,
    PythonDontLoad,
    PythonListen,
    RustFileOps,
    Go21FileOps,
    Go22FileOps,
    Go23FileOps,
    JavaTemurinSip,
    EnvBashCat,
    NodeFileOps,
    NodeSpawn,
    Go21Dir,
    Go22Dir,
    Go23Dir,
    Go21DirBypass,
    Go22DirBypass,
    Go23DirBypass,
    Go21Issue834,
    Go22Issue834,
    Go23Issue834,
    BashShebang,
    Go21Read,
    Go22Read,
    Go23Read,
    Go21Write,
    Go22Write,
    Go23Write,
    Go21LSeek,
    Go22LSeek,
    Go23LSeek,
    Go21FAccessAt,
    Go22FAccessAt,
    Go23FAccessAt,
    Go21SelfOpen,
    Go22SelfOpen,
    Go23SelfOpen,
    RustOutgoingUdp,
    RustOutgoingTcp,
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
    OpenFile,
    CIssue2055,
    CIssue2178,
    RustIssue2058,
    Realpath,
    NodeIssue2283,
    RustIssue2204,
    RustIssue2438,
    RustRebind0,
    // For running applications with the executable and arguments determined at runtime.
    DynamicApp(String, Vec<String>),
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
            Application::Realpath => String::from("tests/apps/realpath/out.c_test_app"),
            Application::NodeHTTP | Application::NodeIssue2283 => String::from("node"),
            Application::JavaTemurinSip => format!(
                "{}/.sdkman/candidates/java/17.0.6-tem/bin/java",
                std::env::var("HOME").unwrap(),
            ),
            Application::Go21HTTP => String::from("tests/apps/app_go/21.go_test_app"),
            Application::Go22HTTP => String::from("tests/apps/app_go/22.go_test_app"),
            Application::Go23HTTP => String::from("tests/apps/app_go/23.go_test_app"),
            Application::Go21FileOps => String::from("tests/apps/fileops/go/21.go_test_app"),
            Application::Go22FileOps => String::from("tests/apps/fileops/go/22.go_test_app"),
            Application::Go23FileOps => String::from("tests/apps/fileops/go/23.go_test_app"),
            Application::RustFileOps => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/fileops"
                )
            }
            Application::EnvBashCat => String::from("tests/apps/env_bash_cat.sh"),
            Application::NodeFileOps | Application::NodeSpawn => String::from("node"),
            Application::Go21Dir => String::from("tests/apps/dir_go/21.go_test_app"),
            Application::Go22Dir => String::from("tests/apps/dir_go/22.go_test_app"),
            Application::Go23Dir => String::from("tests/apps/dir_go/23.go_test_app"),
            Application::Go21Issue834 => String::from("tests/apps/issue834/21.go_test_app"),
            Application::Go22Issue834 => String::from("tests/apps/issue834/22.go_test_app"),
            Application::Go23Issue834 => String::from("tests/apps/issue834/23.go_test_app"),
            Application::Go21DirBypass => String::from("tests/apps/dir_go_bypass/21.go_test_app"),
            Application::Go22DirBypass => String::from("tests/apps/dir_go_bypass/22.go_test_app"),
            Application::Go23DirBypass => String::from("tests/apps/dir_go_bypass/23.go_test_app"),
            Application::BashShebang => String::from("tests/apps/nothing.sh"),
            Application::Go21Read => String::from("tests/apps/read_go/21.go_test_app"),
            Application::Go22Read => String::from("tests/apps/read_go/22.go_test_app"),
            Application::Go23Read => String::from("tests/apps/read_go/23.go_test_app"),
            Application::Go21Write => String::from("tests/apps/write_go/21.go_test_app"),
            Application::Go22Write => String::from("tests/apps/write_go/22.go_test_app"),
            Application::Go23Write => String::from("tests/apps/write_go/23.go_test_app"),
            Application::Go21LSeek => String::from("tests/apps/lseek_go/21.go_test_app"),
            Application::Go22LSeek => String::from("tests/apps/lseek_go/22.go_test_app"),
            Application::Go23LSeek => String::from("tests/apps/lseek_go/23.go_test_app"),
            Application::Go21FAccessAt => String::from("tests/apps/faccessat_go/21.go_test_app"),
            Application::Go22FAccessAt => String::from("tests/apps/faccessat_go/22.go_test_app"),
            Application::Go23FAccessAt => String::from("tests/apps/faccessat_go/23.go_test_app"),
            Application::Go21SelfOpen => String::from("tests/apps/self_open/21.go_test_app"),
            Application::Go22SelfOpen => String::from("tests/apps/self_open/22.go_test_app"),
            Application::Go23SelfOpen => String::from("tests/apps/self_open/23.go_test_app"),
            Application::RustIssue1123 => String::from("tests/apps/issue1123/target/issue1123"),
            Application::RustIssue1054 => String::from("tests/apps/issue1054/target/issue1054"),
            Application::RustIssue1458 => String::from("tests/apps/issue1458/target/issue1458"),
            Application::RustIssue1458PortNot53 => {
                String::from("tests/apps/issue1458portnot53/target/issue1458portnot53")
            }
            Application::RustOutgoingUdp | Application::RustOutgoingTcp => format!(
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
            Application::DynamicApp(exe, _) => exe.clone(),
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
            Application::NodeSpawn => {
                app_path.push("node_spawn.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2283 => {
                app_path.push("issue2883.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::PythonSelfConnect => {
                app_path.push("self_connect.py");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::Go21HTTP
            | Application::Go22HTTP
            | Application::Go23HTTP
            | Application::Go21Dir
            | Application::Go22Dir
            | Application::Go23Dir
            | Application::Go21FileOps
            | Application::Go22FileOps
            | Application::Go23FileOps
            | Application::Go21Issue834
            | Application::Go22Issue834
            | Application::Go23Issue834
            | Application::Go21Read
            | Application::Go22Read
            | Application::Go23Read
            | Application::Go21Write
            | Application::Go22Write
            | Application::Go23Write
            | Application::Go21LSeek
            | Application::Go22LSeek
            | Application::Go23LSeek
            | Application::Go21FAccessAt
            | Application::Go22FAccessAt
            | Application::Go23FAccessAt
            | Application::Fork
            | Application::ReadLink
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
            | Application::Go21SelfOpen
            | Application::Go22SelfOpen
            | Application::Go23SelfOpen
            | Application::Go21DirBypass
            | Application::Go22DirBypass
            | Application::Go23DirBypass
            | Application::RustIssue2058
            | Application::OpenFile
            | Application::CIssue2055
            | Application::CIssue2178
            | Application::RustIssue2204
            | Application::RustRebind0
            | Application::RustIssue2438 => vec![],
            Application::RustOutgoingUdp => ["--udp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::RustOutgoingTcp => ["--tcp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::DynamicApp(_, args) => args.to_owned(),
        }
    }

    pub fn get_app_port(&self) -> u16 {
        match self {
            Application::Go21HTTP
            | Application::Go22HTTP
            | Application::Go23HTTP
            | Application::Go21FileOps
            | Application::Go22FileOps
            | Application::Go23FileOps
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
            | Application::BashShebang
            | Application::Fork
            | Application::ReadLink
            | Application::Realpath
            | Application::Go21Issue834
            | Application::Go22Issue834
            | Application::Go23Issue834
            | Application::Go21Read
            | Application::Go22Read
            | Application::Go23Read
            | Application::Go21Write
            | Application::Go22Write
            | Application::Go23Write
            | Application::Go21LSeek
            | Application::Go22LSeek
            | Application::Go23LSeek
            | Application::Go21FAccessAt
            | Application::Go22FAccessAt
            | Application::Go23FAccessAt
            | Application::Go21DirBypass
            | Application::Go22DirBypass
            | Application::Go23DirBypass
            | Application::Go21SelfOpen
            | Application::Go22SelfOpen
            | Application::Go23SelfOpen
            | Application::Go21Dir
            | Application::Go22Dir
            | Application::Go23Dir
            | Application::RustOutgoingUdp
            | Application::RustOutgoingTcp
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
            | Application::RustRebind0
            | Application::DynamicApp(..) => unimplemented!("shouldn't get here"),
            Application::PythonSelfConnect => 1337,
            Application::RustIssue2058 => 1234,
        }
    }

    /// Start the test process with the given env.
    pub async fn get_test_process(&self, env: HashMap<&str, &str>) -> TestProcess {
        let executable = self.get_executable().await;
        #[cfg(target_os = "macos")]
        let executable = sip_patch(&executable, &Vec::new())
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
        configuration_file: Option<&str>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let env = get_env(
            dylib_path.to_str().unwrap(),
            &address,
            extra_env_vars,
            configuration_file,
        );
        let test_process = self.get_test_process(env).await;

        (test_process, TestIntProxy::new(listener).await)
    }

    /// Like `start_process_with_layer`, but also verify a port subscribe.
    pub async fn start_process_with_layer_and_port(
        &self,
        dylib_path: &Path,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&str>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let env = get_env(
            dylib_path.to_str().unwrap(),
            &address,
            extra_env_vars,
            configuration_file,
        );
        let test_process = self.get_test_process(env).await;

        (
            test_process,
            TestIntProxy::new_with_app_port(listener, self.get_app_port()).await,
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

pub fn get_env<'a>(
    dylib_path_str: &'a str,
    addr: &'a str,
    extra_vars: Vec<(&'a str, &'a str)>,
    config: Option<&'a str>,
) -> HashMap<&'a str, &'a str> {
    let mut env = HashMap::new();
    env.insert("RUST_LOG", "warn,mirrord=debug");
    env.insert("MIRRORD_IMPERSONATED_TARGET", "pod/mock-target"); // Just pass some value.
    env.insert("MIRRORD_CONNECT_TCP", addr);
    env.insert("MIRRORD_REMOTE_DNS", "false");
    if let Some(config) = config {
        println!("using config file: {config}");
        env.insert("MIRRORD_CONFIG_FILE", config);
    }
    env.insert("DYLD_INSERT_LIBRARIES", dylib_path_str);
    env.insert("LD_PRELOAD", dylib_path_str);
    for (key, value) in extra_vars {
        env.insert(key, value);
    }
    env
}

pub fn get_env_no_fs<'a>(dylib_path_str: &'a str, addr: &'a str) -> HashMap<&'a str, &'a str> {
    let mut env = HashMap::new();
    env.insert("RUST_LOG", "warn,mirrord=debug");
    env.insert("MIRRORD_IMPERSONATED_TARGET", "pod/mock-target"); // Just pass some value.
    env.insert("MIRRORD_CONNECT_TCP", addr);
    env.insert("MIRRORD_REMOTE_DNS", "false");
    env.insert("MIRRORD_FILE_MODE", "local");
    env.insert("DYLD_INSERT_LIBRARIES", dylib_path_str);
    env.insert("LD_PRELOAD", dylib_path_str);
    env
}
