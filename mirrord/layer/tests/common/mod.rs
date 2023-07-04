use std::{
    assert_matches::assert_matches, cmp::min, collections::HashMap, fmt::Debug, path::PathBuf,
    process::Stdio, sync::Arc,
};

use actix_codec::Framed;
use chrono::{Timelike, Utc};
use fancy_regex::Regex;
use futures::{SinkExt, StreamExt};
use mirrord_protocol::{
    file::{
        AccessFileRequest, AccessFileResponse, OpenFileRequest, OpenOptionsInternal,
        ReadFileRequest, SeekFromInternal, XstatRequest, XstatResponse,
    },
    tcp::{DaemonTcp, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, DaemonCodec, DaemonMessage, FileRequest, FileResponse,
};
use rstest::fixture;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::Mutex,
};

/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_PEERS: &str = "1.1.1.1:1111,2.2.2.2:2222,3.3.3.3:3333";
/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_LOCAL: &str = "4.4.4.4:4444";

/// Returns string with time format of hh:mm:ss
fn format_time() -> String {
    let now = Utc::now();
    format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
}

pub struct TestProcess {
    pub child: Option<Child>,
    stderr: Arc<Mutex<String>>,
    stdout: Arc<Mutex<String>>,
    error_capture: Regex,
}

impl TestProcess {
    pub async fn get_stdout(&self) -> String {
        self.stdout.lock().await.clone()
    }

    pub async fn get_stderr(&self) -> String {
        self.stderr.lock().await.clone()
    }

    pub async fn assert_log_level(&self, stderr: bool, level: &str) {
        if stderr {
            assert!(!self.stderr.lock().await.contains(level));
        } else {
            assert!(!self.stdout.lock().await.contains(level));
        }
    }

    async fn from_child(mut child: Child) -> TestProcess {
        let stderr_data = Arc::new(Mutex::new(String::new()));
        let stdout_data = Arc::new(Mutex::new(String::new()));
        let child_stderr = child.stderr.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let stderr_data_reader = stderr_data.clone();
        let stdout_data_reader = stdout_data.clone();
        let pid = child.id().unwrap();

        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let string = String::from_utf8_lossy(&buf[..n]);
                eprintln!("stderr {} {pid}: {}", format_time(), string);
                {
                    stderr_data_reader.lock().await.push_str(&string);
                }
            }
        });
        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let string = String::from_utf8_lossy(&buf[..n]);
                print!("stdout {} {pid}: {}", format_time(), string);
                {
                    stdout_data_reader.lock().await.push_str(&string);
                }
            }
        });

        let error_capture = Regex::new(r"^.*ERROR[^\w_-]").unwrap();

        TestProcess {
            child: Some(child),
            stderr: stderr_data,
            stdout: stdout_data,
            error_capture,
        }
    }

    pub async fn start_process(
        executable: String,
        args: Vec<String>,
        env: HashMap<&str, &str>,
    ) -> TestProcess {
        let child = Command::new(executable)
            .args(args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        println!("Started application.");
        TestProcess::from_child(child).await
    }

    pub async fn assert_stdout_contains(&self, string: &str) {
        assert!(self.stdout.lock().await.contains(string));
    }

    pub async fn assert_stderr_contains(&self, string: &str) {
        assert!(self.stderr.lock().await.contains(string));
    }

    pub async fn assert_no_error_in_stdout(&self) {
        assert!(!self
            .error_capture
            .is_match(&self.stdout.lock().await)
            .unwrap());
    }

    pub async fn assert_no_error_in_stderr(&self) {
        assert!(!self
            .error_capture
            .is_match(&self.stderr.lock().await)
            .unwrap());
    }

    pub async fn wait_assert_success(&mut self) {
        let awaited_child = self.child.take();
        let output = awaited_child.unwrap().wait_with_output().await.unwrap();
        assert!(output.status.success());
    }

    pub async fn wait(&mut self) {
        self.child.take().unwrap().wait().await.unwrap();
    }
}

pub struct LayerConnection {
    pub codec: Framed<TcpStream, DaemonCodec>,
    num_connections: u64,
}

impl LayerConnection {
    /// Accept a connection from the layer
    /// Return the codec of the accepted stream.
    async fn accept_library_connection(listener: &TcpListener) -> Framed<TcpStream, DaemonCodec> {
        let (stream, _) = listener.accept().await.unwrap();
        println!("Got connection from library.");
        Framed::new(stream, DaemonCodec::new())
    }

    /// Accept the library's connection and verify initial ENV message
    pub async fn get_initialized_connection(listener: &TcpListener) -> LayerConnection {
        let codec = Self::accept_library_connection(listener).await;
        println!("Got connection from layer.");
        LayerConnection {
            codec,
            num_connections: 0,
        }
    }

    /// Accept the library's connection and verify initial ENV message, `FileRequest` message
    /// (depends on tested program), and PortSubscribe message caused by the listen hook.
    ///
    /// Handle flask's 2 process behaviour.
    pub async fn get_initialized_connection_with_port(
        listener: &TcpListener,
        app_port: u16,
    ) -> LayerConnection {
        let mut codec = Self::accept_library_connection(listener).await;

        // Python runs in 2 processes, only one of which is the application. The library is
        // loaded into both so the first connection will not contain the application and
        // so will not send any of the messages that are generated by the hooks that are
        // triggered by the app.
        // So accept the next connection which will be the one by the library that was
        // loaded to the python process that actually runs the application.
        let (msg, codec) = tokio::select! {
            Some(msg) = codec.next() => (msg, codec),
            mut codec = Self::accept_library_connection(listener) => {
                let msg = codec.next().await.expect("no message received from the second library connection");
                (msg, codec)
            }
        };

        println!("Got first message from library: {:?}", msg);

        match msg.expect("failed to receive message from library") {
            ClientMessage::Tcp(LayerTcp::PortSubscribe(port)) => {
                assert_eq!(app_port, port);
                Self {
                    codec,
                    num_connections: 0,
                }
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
                let mut layer_connection = Self {
                    codec,
                    num_connections: 0,
                };

                layer_connection
                    .handle_gethostname::<false>(Some(app_port))
                    .await;
                layer_connection
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
            let open_file_request = self.codec.next().await?.unwrap();

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

        println!("Should be a close file request: {read_request:#?}");
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

        Some(())
    }

    pub async fn is_ended(&mut self) -> bool {
        if let Some(msg) = self.codec.next().await {
            println!("Got unexpected message: {msg:?}");
            false
        } else {
            true
        }
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
            self.codec.next().await.unwrap().unwrap(),
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
            self.codec.next().await.unwrap().unwrap(),
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
            self.codec.next().await.unwrap().unwrap(),
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
        let read_amount = min(buffer_size, contents.len() as u64);
        let contents = contents.as_bytes()[0..read_amount as usize].to_vec();
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
        let read_amount = min(buffer_size, contents.len() as u64);
        let contents = contents.as_bytes()[0..read_amount as usize].to_vec();
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
            self.codec.next().await.unwrap().unwrap(),
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
            self.codec.next().await.unwrap().unwrap(),
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
        let mut message = self.codec.next().await.unwrap().unwrap();
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
            message = self.codec.next().await.unwrap().unwrap();
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
    Go19HTTP,
    Go20HTTP,
    NodeHTTP,
    PythonFastApiHTTP,
    PythonFlaskHTTP,
    PythonSelfConnect,
    PythonDontLoad,
    PythonListen,
    RustFileOps,
    Go19FileOps,
    Go20FileOps,
    EnvBashCat,
    NodeFileOps,
    NodeSpawn,
    Go19Dir,
    Go20Dir,
    Go19DirBypass,
    Go20DirBypass,
    Go20Issue834,
    Go19Issue834,
    Go18Issue834,
    BashShebang,
    Go18Read,
    Go19Read,
    Go20Read,
    Go18Write,
    Go19Write,
    Go20Write,
    Go18LSeek,
    Go19LSeek,
    Go20LSeek,
    Go18FAccessAt,
    Go19FAccessAt,
    Go20FAccessAt,
    Go19SelfOpen,
    RustOutgoingUdp,
    RustOutgoingTcp,
    RustIssue1123,
    RustIssue1054,
    RustIssue1458,
    RustIssue1458PortNot53,
    RustDnsResolve,
    RustRecvFrom,
    RustListenPorts,
    Fork,
    // For running applications with the executable and arguments determined at runtime.
    // Compiled only on macos just because it's currently only used there, but could be used also
    // on Linux.
    #[cfg(target_os = "macos")]
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
            Application::PythonFastApiHTTP => String::from("uvicorn"),
            Application::Fork => String::from("tests/apps/fork/out.c_test_app"),
            Application::NodeHTTP => String::from("node"),
            Application::Go19HTTP => String::from("tests/apps/app_go/19.go_test_app"),
            Application::Go20HTTP => String::from("tests/apps/app_go/20.go_test_app"),
            Application::Go19FileOps => String::from("tests/apps/fileops/go/19.go_test_app"),
            Application::Go20FileOps => String::from("tests/apps/fileops/go/20.go_test_app"),
            Application::RustFileOps => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/fileops"
                )
            }
            Application::EnvBashCat => String::from("tests/apps/env_bash_cat.sh"),
            Application::NodeFileOps | Application::NodeSpawn => String::from("node"),
            Application::Go19Dir => String::from("tests/apps/dir_go/19.go_test_app"),
            Application::Go20Dir => String::from("tests/apps/dir_go/20.go_test_app"),
            Application::Go20Issue834 => String::from("tests/apps/issue834/20.go_test_app"),
            Application::Go19Issue834 => String::from("tests/apps/issue834/19.go_test_app"),
            Application::Go18Issue834 => String::from("tests/apps/issue834/18.go_test_app"),
            Application::Go19DirBypass => String::from("tests/apps/dir_go_bypass/19.go_test_app"),
            Application::Go20DirBypass => String::from("tests/apps/dir_go_bypass/20.go_test_app"),
            Application::BashShebang => String::from("tests/apps/nothing.sh"),
            Application::Go18Read => String::from("tests/apps/read_go/18.go_test_app"),
            Application::Go19Read => String::from("tests/apps/read_go/19.go_test_app"),
            Application::Go20Read => String::from("tests/apps/read_go/20.go_test_app"),
            Application::Go18Write => String::from("tests/apps/write_go/18.go_test_app"),
            Application::Go19Write => String::from("tests/apps/write_go/19.go_test_app"),
            Application::Go20Write => String::from("tests/apps/write_go/20.go_test_app"),
            Application::Go18LSeek => String::from("tests/apps/lseek_go/18.go_test_app"),
            Application::Go19LSeek => String::from("tests/apps/lseek_go/19.go_test_app"),
            Application::Go20LSeek => String::from("tests/apps/lseek_go/20.go_test_app"),
            Application::Go18FAccessAt => String::from("tests/apps/faccessat_go/18.go_test_app"),
            Application::Go19FAccessAt => String::from("tests/apps/faccessat_go/19.go_test_app"),
            Application::Go20FAccessAt => String::from("tests/apps/faccessat_go/20.go_test_app"),
            Application::Go19SelfOpen => String::from("tests/apps/self_open/19.go_test_app"),
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
            #[cfg(target_os = "macos")]
            Application::DynamicApp(exe, _args) => exe.clone(),
        }
    }

    pub fn get_args(&self) -> Vec<String> {
        let mut app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        app_path.push("tests/apps/");
        match self {
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
            Application::PythonSelfConnect => {
                app_path.push("self_connect.py");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::Go19HTTP
            | Application::Go20HTTP
            | Application::Go19Dir
            | Application::Go20Dir
            | Application::Go19FileOps
            | Application::Go20FileOps
            | Application::Go20Issue834
            | Application::Go19Issue834
            | Application::Go18Issue834
            | Application::Go20Read
            | Application::Go19Read
            | Application::Go18Read
            | Application::Go20Write
            | Application::Go19Write
            | Application::Go18Write
            | Application::Go20LSeek
            | Application::Go19LSeek
            | Application::Go18LSeek
            | Application::Go20FAccessAt
            | Application::Go19FAccessAt
            | Application::Go18FAccessAt
            | Application::Fork
            | Application::RustFileOps
            | Application::RustIssue1123
            | Application::RustIssue1054
            | Application::RustIssue1458
            | Application::RustIssue1458PortNot53
            | Application::RustDnsResolve
            | Application::RustRecvFrom
            | Application::RustListenPorts
            | Application::EnvBashCat
            | Application::BashShebang
            | Application::Go19SelfOpen
            | Application::Go19DirBypass
            | Application::Go20DirBypass => vec![],
            Application::RustOutgoingUdp => ["--udp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::RustOutgoingTcp => ["--tcp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            #[cfg(target_os = "macos")]
            Application::DynamicApp(_exe, args) => args.to_owned(),
        }
    }

    pub fn get_app_port(&self) -> u16 {
        match self {
            Application::Go19HTTP
            | Application::Go20HTTP
            | Application::Go19FileOps
            | Application::Go20FileOps
            | Application::NodeHTTP
            | Application::RustIssue1123
            | Application::RustIssue1054
            | Application::PythonFlaskHTTP => 80,
            Application::PythonFastApiHTTP => 1234,
            Application::PythonListen => 21232,
            Application::PythonDontLoad
            | Application::RustFileOps
            | Application::RustDnsResolve
            | Application::EnvBashCat
            | Application::NodeFileOps
            | Application::NodeSpawn
            | Application::BashShebang
            | Application::Fork
            | Application::Go20Issue834
            | Application::Go19Issue834
            | Application::Go18Issue834
            | Application::Go20Read
            | Application::Go19Read
            | Application::Go18Read
            | Application::Go20Write
            | Application::Go19Write
            | Application::Go18Write
            | Application::Go20LSeek
            | Application::Go19LSeek
            | Application::Go18LSeek
            | Application::Go20FAccessAt
            | Application::Go19FAccessAt
            | Application::Go18FAccessAt
            | Application::Go19DirBypass
            | Application::Go20DirBypass
            | Application::Go19SelfOpen
            | Application::Go19Dir
            | Application::Go20Dir
            | Application::RustOutgoingUdp
            | Application::RustOutgoingTcp
            | Application::RustIssue1458
            | Application::RustIssue1458PortNot53
            | Application::RustListenPorts
            | Application::RustRecvFrom => unimplemented!("shouldn't get here"),
            #[cfg(target_os = "macos")]
            Application::DynamicApp(_, _) => unimplemented!("shouldn't get here"),
            Application::PythonSelfConnect => 1337,
        }
    }

    /// Start the test process with the given env.
    async fn get_test_process(&self, env: HashMap<&str, &str>) -> TestProcess {
        let executable = self.get_executable().await;
        println!("Using executable: {}", &executable);
        println!("Using args: {:?}", self.get_args());
        TestProcess::start_process(executable, self.get_args(), env).await
    }

    /// Start the test process and return the started process and a tcp listener that the layer is
    /// supposed to connect to.
    pub async fn get_test_process_and_listener(
        &self,
        dylib_path: &PathBuf,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&str>,
    ) -> (TestProcess, TcpListener) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        println!("Listening for messages from the layer on {addr}");
        let env = get_env(
            dylib_path.to_str().unwrap(),
            &addr,
            extra_env_vars,
            configuration_file,
        );
        let test_process = self.get_test_process(env).await;

        (test_process, listener)
    }

    /// Start the process of this application, with the layer lib loaded.
    /// Will start it with env from `get_env` plus whatever is passed in `extra_env_vars`.
    pub async fn start_process_with_layer(
        &self,
        dylib_path: &PathBuf,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&str>,
    ) -> (TestProcess, LayerConnection) {
        let (test_process, listener) = self
            .get_test_process_and_listener(dylib_path, extra_env_vars, configuration_file)
            .await;
        let layer_connection = LayerConnection::get_initialized_connection(&listener).await;
        (test_process, layer_connection)
    }

    /// Like `start_process_with_layer`, but also verify a port subscribe.
    pub async fn start_process_with_layer_and_port(
        &self,
        dylib_path: &PathBuf,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&str>,
    ) -> (TestProcess, LayerConnection) {
        let (test_process, listener) = self
            .get_test_process_and_listener(dylib_path, extra_env_vars, configuration_file)
            .await;

        let layer_connection =
            LayerConnection::get_initialized_connection_with_port(&listener, self.get_app_port())
                .await;

        (test_process, layer_connection)
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
