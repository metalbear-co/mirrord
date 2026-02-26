use std::{
    assert_matches::assert_matches,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use actix_codec::Framed;
use futures::{SinkExt, StreamExt};
use mirrord_config::{
    LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
    experimental::ExperimentalFileConfig,
};
use mirrord_intproxy::{IntProxy, agent_conn::AgentConnection};
#[cfg(doc)]
use mirrord_intproxy::LayerConnection;
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
use tokio::net::{TcpListener, TcpStream};

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

    /// Makes a [`FileRequest::StatFsV2`] and answers it.
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

    /// Makes a [`FileRequest::XstatFsV2`] and answers it.
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
