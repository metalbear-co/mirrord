// #![warn(missing_docs)]
// #![warn(rustdoc::missing_crate_level_docs)]

use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::{body::Incoming, Request};
use mirrord_protocol::ConnectionId;
use tokio::{net::TcpStream, sync::mpsc::Sender};

use self::{
    error::HttpTrafficError,
    filter::{HttpFilter, HttpFilterBuilder, MINIMAL_HEADER_SIZE},
    reversable_stream::ReversableStream,
};
use crate::{steal::StealerHttpRequest, util::ClientId};

pub(crate) mod error;
pub(super) mod filter;
mod hyper_handler;
pub(super) mod reversable_stream;

pub(super) type DefaultReversableStream = ReversableStream<MINIMAL_HEADER_SIZE>;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    #[default]
    V1,
    V2,
    NotHttp,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    #[tracing::instrument(level = "debug")]
    fn new(buffer: &[u8], h2_preface: &[u8]) -> Self {
        println!(
            "buffer {:#?} | h2_preface {:#?}",
            String::from_utf8_lossy(buffer),
            h2_preface
        );
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer == h2_preface {
            println!("HTTP2");
            Self::V2
        } else if matches!(
            httparse::Request::new(&mut empty_headers).parse(buffer),
            Ok(_) | Err(httparse::Error::TooManyHeaders)
        ) {
            println!("HTTP1");
            Self::V1
        } else {
            println!("Not http!");
            Self::NotHttp
        }
    }
}

#[derive(Debug)]
pub struct PassthroughRequest(Request<Incoming>);

/// Created for every new port we want to filter HTTP traffic on.
#[derive(Debug)]
pub(super) struct HttpFilterManager {
    // TODO(alex) [low] 2022-12-12: Probably don't need this, adding for debugging right now.
    port: u16,
    client_filters: Arc<DashMap<ClientId, Regex>>,

    /// We clone this to pass them down to the hyper tasks.
    captured_tx: Sender<StealerHttpRequest>,
    passthrough_tx: Sender<PassthroughRequest>,
}

impl HttpFilterManager {
    /// Creates a new [`HttpFilterManager`] per port.
    ///
    /// You can't create just an empty [`HttpFilterManager`], as we don't steal traffic on ports
    /// that no client has registered interest in.
    pub(super) fn new(
        port: u16,
        client_id: ClientId,
        filter: Regex,
        captured_tx: Sender<StealerHttpRequest>,
        passthrough_tx: Sender<PassthroughRequest>,
    ) -> Self {
        let client_filters = Arc::new(DashMap::with_capacity(128));
        client_filters
            .insert(client_id, filter)
            .inspect(|foo| println!("{:#?}", foo));

        Self {
            port,
            client_filters,
            captured_tx,
            passthrough_tx,
        }
    }

    // TODO(alex) [high] 2022-12-12: Is adding a filter like this enough for it to be added to the
    // hyper task? Do we have a possible deadlock here? Tune in next week for the conclusion!
    //
    /// Inserts a new client (layer) and its filter.
    ///
    /// [`HttpFilterManager::client_filters`] are shared between hyper tasks, so adding a new one
    /// here will impact the tasks as well.
    pub(super) fn new_client(&mut self, client_id: ClientId, filter: Regex) -> Option<Regex> {
        self.client_filters.insert(client_id, filter)
    }

    /// Removes a client (layer) from [`HttpFilterManager::client_filters`].
    ///
    /// [`HttpFilterManager::client_filters`] are shared between hyper tasks, so removing a client
    /// here will impact the tasks as well.
    pub(super) fn remove_client(&mut self, client_id: &ClientId) -> Option<(ClientId, Regex)> {
        self.client_filters.remove(client_id)
    }

    pub(super) fn contains_client(&self, client_id: &ClientId) -> bool {
        self.client_filters.contains_key(client_id)
    }

    // TODO(alex) [high] 2022-12-12: hyper doesn't take the actual stream, we're going to be
    // separating it in reader/writer, so hyper can just return empty responses to nowhere (we glue
    // a writer from a duplex channel to the actual reader from TcpStream).
    //
    // If it matches the filter, we send this request via a channel to the layer. And on the
    // Manager, we wait for a message from the layer to send on the writer side of the actual
    // TcpStream.
    //
    /// Starts a new hyper task if the `connection` contains a _valid-ish_ HTTP request.
    ///
    /// The [`TcpStream`] itself os not what we feed hyper, instead we create a [`DuplexStream`],
    /// where one half (_server_) is where hyper does its magic, while the other half
    /// (_interceptor_) sends the bytes we get from the remote connection.
    ///
    /// The _interceptor_ stream is fed the bytes we're reading from the _original_ [`TcpStream`],
    /// and sends them to the _server_ stream.
    ///
    /// This mechanism is required to avoid having hyper send back [`Response`]s to the remote
    /// connection.
    pub(super) async fn new_connection(
        &self,
        original_stream: TcpStream,
        original_address: SocketAddr,
        connection_id: ConnectionId,
    ) -> Result<Option<HttpFilter>, HttpTrafficError> {
        HttpFilterBuilder::new(
            original_stream,
            original_address,
            connection_id,
            self.client_filters.clone(),
            self.captured_tx.clone(),
            self.passthrough_tx.clone(),
        )
        .await?
        .start()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.client_filters.is_empty()
    }
}

#[cfg(test)]
mod http_traffic_tests {
    use std::net::Ipv4Addr;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        select,
        sync::mpsc::channel,
    };

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_traffic_filter_selects_on_header() {
        let server_address: SocketAddr = (Ipv4Addr::LOCALHOST, 7777).into();
        let server = TcpListener::bind(server_address)
            .await
            .expect("Bound TcpListener.");

        let request_task = tokio::spawn(async move {
            let client = reqwest::Client::new();
            let request = client
                .get(format!("http://127.0.0.1:{}", server_address.port()))
                .header("First-Header", "mirrord")
                .header("Mirrord-Test", "Hello")
                .build()
                .unwrap();

            // Send a request and wait compare the dummy response we get from the filter's hyper
            // handler.
            let response = client.execute(request).await.unwrap();
            assert_eq!(response.text().await.unwrap(), "Captured!".to_string());
        });

        let (tcp_stream, _) = server.accept().await.expect("Connection success!");

        let client_id = 1;
        let filter = Regex::new("Hello").expect("Valid regex.");

        let (captured_tx, mut captured_rx) = channel(15000);
        let (passthrough_tx, _) = channel(15000);

        let http_filter_manager = HttpFilterManager::new(
            tcp_stream.local_addr().unwrap().port(),
            client_id,
            filter,
            captured_tx,
            passthrough_tx,
        );

        let HttpFilter {
            hyper_task,
            mut reversable_stream,
            mut interceptor_stream,
        } = http_filter_manager
            .new_connection(tcp_stream, server_address, 0)
            .await
            .unwrap()
            .unwrap();

        let mut interceptor_buffer = vec![0; 15000];

        loop {
            select! {
                // Server stream reads what it received from the client (remote app), and sends it
                // to the hyper task via the intermmediate DuplexStream.
                Ok(read) = reversable_stream.read(&mut interceptor_buffer) => {
                    if read == 0 {
                        break;
                    }

                    let wrote = interceptor_stream.write(&interceptor_buffer[..read]).await.unwrap();
                    assert_eq!(wrote, read);
                }

                // Receives captured requests from the hyper task.
                Some(_) = captured_rx.recv() => {
                    // Send the dummy response from hyper to our client, so it can stop blocking
                    // and exit.
                    let mut response_buffer = vec![0;1500];
                    let read_amount = interceptor_stream.read(&mut response_buffer).await.unwrap();
                    reversable_stream.write(&response_buffer[..read_amount]).await.unwrap();

                    break;
                }

                else => {
                    break;
                }
            }
        }

        // Manually close this stream to notify the filter's hyper handler that this connection is
        // over.
        drop(interceptor_stream);

        assert!(hyper_task.await.is_ok());
        assert!(request_task.await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_traffic_filter_total_passthrough_not_http() {
        let server_address: SocketAddr = (Ipv4Addr::LOCALHOST, 8888).into();
        let server = TcpListener::bind(server_address)
            .await
            .expect("Bound TcpListener.");

        let request_task = tokio::spawn(async move {
            let message =
                "Hey / friend this is not an HTTP message! Don't even filter it, ok?".to_string();
            let mut client = TcpStream::connect(server_address).await.unwrap();

            let wrote = client.write(message.as_bytes()).await.unwrap();
            assert_eq!(wrote, message.len());
        });

        let (tcp_stream, _) = server.accept().await.expect("Connection success!");

        let client_id = 1;
        let filter = Regex::new("Hello").expect("Valid regex.");

        let (captured_tx, _) = channel(15000);
        let (passthrough_tx, _) = channel(15000);

        let http_filter_manager = HttpFilterManager::new(
            tcp_stream.local_addr().unwrap().port(),
            client_id,
            filter,
            captured_tx,
            passthrough_tx,
        );

        if let None = http_filter_manager
            .new_connection(tcp_stream, server_address, 0)
            .await
            .unwrap()
        {
        } else {
            panic!("`http_filter_manager.new_connection` has to be `None` here!");
        }

        assert!(request_task.await.is_ok());
    }
}
