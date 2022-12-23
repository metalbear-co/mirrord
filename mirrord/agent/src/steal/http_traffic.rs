// #![warn(missing_docs)]
// #![warn(rustdoc::missing_crate_level_docs)]

use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use mirrord_protocol::{tcp::HttpResponse, ConnectionId};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};

use self::{
    error::HttpTrafficError,
    filter::{HttpFilter, HttpFilterBuilder, MINIMAL_HEADER_SIZE},
    reversible_stream::ReversibleStream,
};
use crate::{steal::MatchedHttpRequest, util::ClientId};

pub(crate) mod error;
pub(super) mod filter;
mod hyper_handler;
pub(super) mod reversible_stream;

pub(crate) type UnmatchedSender = Sender<Result<UnmatchedHttpResponse, HttpTrafficError>>;
pub(crate) type UnmatchedReceiver = Receiver<Result<UnmatchedHttpResponse, HttpTrafficError>>;

pub(super) type DefaultReversibleStream = ReversibleStream<MINIMAL_HEADER_SIZE>;

/// Identifies a message as being HTTP or not.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    #[default]
    V1,
    V2,

    /// Handled as a special passthrough case, where the captured stream just forwards messages to
    /// their original destination (and vice-versa).
    NotHttp,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    #[tracing::instrument(level = "debug")]
    fn new(buffer: &[u8], h2_preface: &[u8]) -> Self {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer == h2_preface {
            Self::V2
        } else if matches!(
            httparse::Request::new(&mut empty_headers).parse(buffer),
            Ok(_) | Err(httparse::Error::TooManyHeaders)
        ) {
            Self::V1
        } else {
            Self::NotHttp
        }
    }
}

#[derive(Debug)]
pub struct UnmatchedHttpResponse(pub(super) HttpResponse);

/// Created for every new port we want to filter HTTP traffic on.
#[derive(Debug)]
pub(super) struct HttpFilterManager {
    _port: u16,
    client_filters: Arc<DashMap<ClientId, Regex>>,

    /// We clone this to pass them down to the hyper tasks.
    matched_tx: Sender<MatchedHttpRequest>,
    unmatched_tx: UnmatchedSender,
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
        matched_tx: Sender<MatchedHttpRequest>,
        unmatched_tx: UnmatchedSender,
    ) -> Self {
        let client_filters = Arc::new(DashMap::with_capacity(128));
        client_filters.insert(client_id, filter);

        Self {
            _port: port,
            client_filters,
            matched_tx,
            unmatched_tx,
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
            self.matched_tx.clone(),
            self.unmatched_tx.clone(),
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
    use core::convert::Infallible;
    use std::net::Ipv4Addr;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{
        body::Incoming, client, server::conn::http1, service::service_fn, Request, Response,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        select,
        sync::mpsc::channel,
    };

    use super::*;
    use crate::steal::http_traffic::hyper_handler::{
        DUMMY_RESPONSE_MATCHED, DUMMY_RESPONSE_UNMATCHED,
    };

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
            assert_eq!(
                response.text().await.unwrap(),
                DUMMY_RESPONSE_MATCHED.to_string()
            );
        });

        let (tcp_stream, _) = server.accept().await.expect("Connection success!");

        let client_id = 1;
        let filter = Regex::new("Hello").expect("Valid regex.");

        let (matched_tx, mut matched_rx) = channel(15000);
        let (unmatched_tx, _) = channel(15000);

        let http_filter_manager = HttpFilterManager::new(
            tcp_stream.local_addr().unwrap().port(),
            client_id,
            filter,
            matched_tx,
            unmatched_tx,
        );

        let HttpFilter {
            _hyper_task,
            mut reversible_stream,
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
                Ok(read) = reversible_stream.read(&mut interceptor_buffer) => {
                    if read == 0 {
                        break;
                    }

                    let wrote = interceptor_stream.write(&interceptor_buffer[..read]).await.unwrap();
                    assert_eq!(wrote, read);
                }

                // Receives matched requests from the hyper task.
                Some(_) = matched_rx.recv() => {
                    // Send the dummy response from hyper to our client, so it can stop blocking
                    // and exit.
                    let mut response_buffer = vec![0;1500];
                    let read_amount = interceptor_stream.read(&mut response_buffer).await.unwrap();
                    reversible_stream.write(&response_buffer[..read_amount]).await.unwrap();

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

        assert!(_hyper_task.await.is_ok());
        assert!(request_task.await.is_ok());
    }

    /// Replies with a proper `Response` to test the unmatched filter case.
    async fn dummy_reply(_req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
        Ok(Response::new(Full::new(Bytes::from("Hello World!"))))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_traffic_filter_no_headers_match_the_request() {
        let server_address: SocketAddr = (Ipv4Addr::LOCALHOST, 8888).into();
        tokio::spawn(async move {
            let server = TcpListener::bind(server_address)
                .await
                .expect("Bound TcpListener.");

            let (server_stream, _) = server.accept().await.expect("Connection success!");

            if let Err(http_err) = http1::Builder::new()
                .http1_keep_alive(true)
                .serve_connection(server_stream, service_fn(dummy_reply))
                .await
            {
                eprintln!("Error while serving HTTP connection: {}", http_err);
            }
        });

        let agent_address: SocketAddr = (Ipv4Addr::LOCALHOST, 8889).into();
        let agent = TcpListener::bind(agent_address)
            .await
            .expect("Bound TcpListener.");

        // The client sends a request to the agent (mimmicking the stealing feature).
        let client_task = tokio::spawn(async move {
            let client = TcpStream::connect(agent_address).await.unwrap();
            let (mut request_sender, connection) =
                client::conn::http1::handshake(client).await.unwrap();

            tokio::spawn(async move {
                if let Err(fail) = connection.await {
                    eprintln!("Error in connection: {}", fail);
                }
            });

            let body = Bytes::from("Hello, HTTP!".to_string().into_bytes());
            let body = Full::new(body);
            let request = Request::builder()
                .method("GET")
                .uri(format!("http://127.0.0.1:{}", server_address.port()))
                .header("First-Header", "mirrord")
                .header("Mirrord-Test", "Hello")
                .body(body)
                .unwrap();

            // Send a request and wait compare the dummy response we get from the filter's hyper
            // handler.
            let response_body = request_sender
                .send_request(request)
                .await
                .unwrap()
                .into_body();

            let got_body = response_body.collect().await.unwrap().to_bytes();
            let expected = Bytes::from(DUMMY_RESPONSE_UNMATCHED.to_string().into_bytes());
            assert_eq!(got_body, expected);
        });

        let (tcp_stream, _) = agent.accept().await.expect("Connection success!");

        let client_id = 1;
        let filter = Regex::new("Goodbye").expect("Valid regex.");

        let (matched_tx, _) = channel(15000);
        let (unmatched_tx, mut unmatched_rx) = channel(15000);

        let http_filter_manager = HttpFilterManager::new(
            tcp_stream.local_addr().unwrap().port(),
            client_id,
            filter,
            matched_tx,
            unmatched_tx,
        );

        // The filter is created with the original address being the server (the agent "steals" from
        // the server).
        let HttpFilter {
            _hyper_task,
            mut reversible_stream,
            mut interceptor_stream,
        } = http_filter_manager
            .new_connection(tcp_stream, server_address, 0)
            .await
            .unwrap()
            .unwrap();

        let mut buffer = vec![0; 15000];

        loop {
            select! {
                // Server stream reads what it received from the client (remote app), and sends it
                // to the hyper task via the intermmediate DuplexStream.
                Ok(read) = reversible_stream.read(&mut buffer) => {
                    if read == 0 {
                        break;
                    }

                    println!("agent received {:#?}", String::from_utf8_lossy(&buffer[..read]));
                    let wrote = interceptor_stream.write(&buffer[..read]).await.unwrap();
                    assert_eq!(wrote, read);
                }

                // Receives requests from the hyper task that did not match any filter.
                Some(response) = unmatched_rx.recv() => {
                    println!("Have some data here {:#?}", response);
                    // Send the dummy response from hyper to our client, so it can stop blocking
                    // and exit.
                    let mut response_buffer = vec![0;1500];
                    let read_amount = interceptor_stream.read(&mut response_buffer).await.unwrap();
                    reversible_stream.write(&response_buffer[..read_amount]).await.unwrap();

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

        assert!(_hyper_task.await.is_ok());
        assert!(client_task.await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_traffic_filter_total_passthrough_not_http() {
        let server_address: SocketAddr = (Ipv4Addr::LOCALHOST, 9999).into();
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

        let (matched_tx, _) = channel(15000);
        let (unmatched_tx, _) = channel(15000);

        let http_filter_manager = HttpFilterManager::new(
            tcp_stream.local_addr().unwrap().port(),
            client_id,
            filter,
            matched_tx,
            unmatched_tx,
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
