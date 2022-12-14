// #![warn(missing_docs)]
// #![warn(rustdoc::missing_crate_level_docs)]

use core::{future::Future, pin::Pin};
use std::{io, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use hyper::{
    body::Incoming,
    server::conn::{http1, http2},
    service::Service,
    Request, Response,
};
use thiserror::Error;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    sync::mpsc::{channel, Receiver, Sender},
    task::JoinHandle,
};
use tracing::error;

use crate::util::ClientId;

#[derive(Error, Debug)]
pub(super) enum HttpError {
    #[error("Failed parsing HTTP with 0 bytes!")]
    Empty,

    #[error("Failed client not found `{0}`!")]
    ClientNotFound(ClientId),

    #[error("Failed parsing HTTP smaller than minimal!")]
    TooSmall,

    #[error("Failed as the buffer does not contain a valid HTTP request!")]
    NotHttp,

    #[error("Failed with IO `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("Failed with Parse `{0}`!")]
    Parse(#[from] httparse::Error),

    #[error("Failed with Hyper `{0}`!")]
    Hyper(#[from] hyper::Error),

    #[error("Failed with Captured `{0}`!")]
    CapturedSender(#[from] tokio::sync::mpsc::error::SendError<CapturedRequest>),

    #[error("Failed with Passthrough `{0}`!")]
    PassthroughSender(#[from] tokio::sync::mpsc::error::SendError<PassthroughRequest>),
}

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    #[default]
    V1,
    V2,
}

struct HttpFilterBuilder {
    http_version: HttpVersion,
    original_stream: TcpStream,
    hyper_stream: DuplexStream,
    interceptor_stream: DuplexStream,
    client_filters: Arc<DashMap<ClientId, Regex>>,
    captured_tx: Sender<CapturedRequest>,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    fn new(buffer: &[u8], h2_preface: &[u8]) -> Result<Self, HttpError> {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer == h2_preface {
            Ok(Self::V2)
        } else if matches!(
            httparse::Request::new(&mut empty_headers).parse(buffer),
            Ok(_) | Err(httparse::Error::TooManyHeaders)
        ) {
            Ok(Self::V1)
        } else {
            Err(HttpError::NotHttp)
        }
    }
}

#[derive(Debug)]
pub(super) struct CapturedRequest {
    client_id: ClientId,
    request: Request<Incoming>,
}

#[derive(Debug)]
pub(super) struct PassthroughRequest(Request<Incoming>);

struct HyperFilter {
    filters: Arc<DashMap<ClientId, Regex>>,
    captured_tx: Sender<CapturedRequest>,
    passthrough_tx: Sender<PassthroughRequest>,
}

// TODO(alex) [low] 2022-12-13: Come back to these docs to create a link to where this is in the
// agent.
/// Creates a task to send a message (either [`CapturedRequest`] or [`PassthroughRequest`]) to the
/// receiving end that lives in the stealer.
///
/// As the [`hyper::service::Service`] trait doesn't support `async fn` for the [`Service::call`]
/// method, we use this helper function that allows us to send a `value: T` via a `Sender<T>`
/// without the need to call `await`.
fn spawn_send<T>(value: T, tx: Sender<T>)
where
    T: Send + 'static,
    HttpError: From<tokio::sync::mpsc::error::SendError<T>>,
{
    tokio::spawn(async move {
        println!(
            "Yay we sent the request {:#?}",
            tx.send(value).map_err(HttpError::from).await
        );
    });
}

impl Service<Request<Incoming>> for HyperFilter {
    type Response = Response<String>;

    type Error = HttpError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        println!("hyper request \n{:#?}", request);

        // TODO(alex) [mid] 2022-12-13: Do we care at all about what is sent from here as a
        // response to our client duplex stream?
        let response = async { Ok(Response::new("async is love".to_string())) };

        if let Some(client_id) = request
            .headers()
            .iter()
            .map(|(header_name, header_value)| {
                format!("{}={}", header_name, header_value.to_str().unwrap())
            })
            .find_map(|header| {
                self.filters.iter().find_map(|filter| {
                    if filter.is_match(&header).unwrap() {
                        Some(filter.key().clone())
                    } else {
                        None
                    }
                })
            })
        {
            println!("We have a captured request {:#?}", request);
            spawn_send(
                CapturedRequest { client_id, request },
                self.captured_tx.clone(),
            );

            Box::pin(response)
        } else {
            println!("This is a passthrough {:#?}", request);
            spawn_send(PassthroughRequest(request), self.passthrough_tx.clone());

            Box::pin(response)
        }
    }
}

impl HttpFilterBuilder {
    /// Does not consume bytes from the stream.
    ///
    /// Checks if the first available bytes in a stream could be of an http request.
    ///
    /// This is a best effort classification, not a guarantee that the stream is HTTP.
    async fn new(
        tcp_stream: TcpStream,
        filters: Arc<DashMap<ClientId, Regex>>,
        captured_tx: Sender<CapturedRequest>,
    ) -> Result<Self, HttpError> {
        let mut buffer = [0u8; 64];
        // TODO(alex) [mid] 2022-12-09: Maybe we do need `poll_peek` here, otherwise just `peek`
        // might return 0 bytes peeked.
        match tcp_stream
            .peek(&mut buffer)
            .await
            .map_err(From::from)
            .and_then(|peeked_amount| {
                if peeked_amount == 0 {
                    Err(HttpError::Empty)
                } else {
                    HttpVersion::new(
                        &buffer[..peeked_amount],
                        &H2_PREFACE[..peeked_amount.min(H2_PREFACE.len())],
                    )
                }
            }) {
            Ok(http_version) => {
                let (hyper_stream, interceptor_stream) = duplex(15000);

                Ok(Self {
                    http_version,
                    client_filters: filters,
                    original_stream: tcp_stream,
                    hyper_stream,
                    interceptor_stream,
                    captured_tx,
                })
            }
            // TODO(alex) [mid] 2022-12-09: This whole filter is a passthrough case.
            Err(HttpError::NotHttp) => todo!(),
            Err(fail) => {
                error!("Something went wrong in http filter {fail:#?}");
                Err(fail)
            }
        }
    }

    /// Creates the hyper task, and returns an [`HttpFilter`] that contains the channels we use to
    /// pass the requests to the layer.
    fn start(self) -> Result<HttpFilter, HttpError> {
        let Self {
            http_version,
            original_stream,
            hyper_stream,
            interceptor_stream,
            client_filters,
            captured_tx,
        } = self;

        let (passthrough_tx, passthrough_rx) = channel(1500);

        let hyper_task = match http_version {
            HttpVersion::V1 => tokio::task::spawn(async move {
                http1::Builder::new()
                    .serve_connection(
                        hyper_stream,
                        HyperFilter {
                            filters: client_filters,
                            captured_tx,
                            passthrough_tx,
                        },
                    )
                    .await
                    .map_err(From::from)
            }),
            // TODO(alex) [mid] 2022-12-09: http2 builder wants an executor?
            HttpVersion::V2 => {
                http2::Builder::new(todo!());

                todo!()
            }
        };

        Ok(HttpFilter {
            hyper_task,
            original_stream,
            interceptor_stream,
            passthrough_rx,
        })
    }
}

/// Used by the stealer handler to:
///
/// 1. Read the requests from hyper's channels through [`captured_rx`], and [`passthrough_rx`];
/// 2. Send the raw bytes we got from the remote connection to hyper through [`interceptor_stream`];
pub(super) struct HttpFilter {
    hyper_task: JoinHandle<Result<(), HttpError>>,
    /// The original [`TcpStream`] that is connected to us, this is where we receive the requests
    /// from.
    original_stream: TcpStream,

    /// A stream that we use to communicate with the hyper task.
    ///
    /// Don't ever [`DuplexStream::read`] anything from it, as the hyper task only responds with
    /// garbage (treat the `read` side as `/dev/null`).
    ///
    /// We use [`DuplexStream::write`] to write the bytes we have `read` from [`original_stream`]
    /// to the hyper task, acting as a "client".
    interceptor_stream: DuplexStream,
    passthrough_rx: Receiver<PassthroughRequest>,
}

impl HttpFilter {
    async fn intercept_from_client(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read_amount = self.original_stream.read(buffer).await?;
        self.interceptor_stream.write(&buffer[..read_amount]).await
    }
}

/// Created for every new port we want to filter HTTP traffic on.
pub(super) struct HttpFilterManager {
    // TODO(alex) [low] 2022-12-12: Probably don't need this, adding for debugging right now.
    port: u16,
    client_filters: Arc<DashMap<ClientId, Regex>>,

    /// We clone this to pass them down to the hyper tasks.
    captured_tx: Sender<CapturedRequest>,
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
        captured_tx: Sender<CapturedRequest>,
    ) -> Self {
        let client_filters = Arc::new(DashMap::with_capacity(128));
        client_filters
            .insert(client_id, filter)
            .inspect(|foo| println!("{:#?}", foo));

        Self {
            port,
            client_filters,
            captured_tx,
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
        connection: TcpStream,
    ) -> Result<HttpFilter, HttpError> {
        HttpFilterBuilder::new(
            connection,
            self.client_filters.clone(),
            self.captured_tx.clone(),
        )
        .await?
        .start()
    }
}

#[cfg(test)]
mod http_traffic_tests {
    use std::{
        collections::HashMap,
        net::{IpAddr, Ipv4Addr},
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        select,
    };

    use super::*;

    // TODO(alex) [mid] 2022-12-14: Be smart, use `reqwest` instead of creating your own tcp
    // connection and request thingies.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_traffic_filter_selects_on_header() {
        let mut hello_request = r#"
GET / HTTP/1.1
Host: mirrord.local
First-Header: mirrord
Mirrord-Test: Hello

"#
        .to_string()
        .into_bytes();

        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 7777))
            .await
            .expect("Bound TcpListener.");
        println!("Server created!");

        tokio::spawn(async move {
            let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, 7777))
                .await
                .expect("Connected TcpStream.");
            println!("Client connected!");

            let client_wrote = client.write(&mut hello_request).await.unwrap();
            assert_eq!(client_wrote, hello_request.len());
        });

        let (tcp_stream, _) = server.accept().await.expect("Connection success!");
        println!("Server accepted connection!");

        let mut interceptor_buffer = vec![0; 15000];

        let client_id = 1;
        let filter = Regex::new("Hello").expect("Valid regex.");

        let (captured_tx, mut captured_rx) = channel(15000);
        let http_filter_manager = HttpFilterManager::new(
            tcp_stream.local_addr().unwrap().port(),
            client_id,
            filter,
            captured_tx,
        );

        let HttpFilter {
            hyper_task,
            mut original_stream,
            mut interceptor_stream,
            passthrough_rx,
        } = http_filter_manager
            .new_connection(tcp_stream)
            .await
            .unwrap();

        // TODO(alex) [mid] 2022-12-14: We only ever break out of this loop if the client ->
        // interceptor -> captured mechanism worked, any deviation will keep this running forever.
        //
        // If we try to break on `0` bytes read, we might break the loop too soon?
        loop {
            select! {
                // Server stream reads what it received from the client (remote app), and sends it
                // to the hyper task via the intermmediate DuplexStream.
                Ok(read) = original_stream.read(&mut interceptor_buffer) => {
                    // if read == 0 {
                    //     break;
                    // }
                    // println!("read {:#?} bytes {:#?}", read, &interceptor_buffer[..read]);

                    let wrote = interceptor_stream.write(&interceptor_buffer[..read]).await.unwrap();
                    assert_eq!(wrote, read);
                }

                // Receives captured requests from the hyper task.
                captured = captured_rx.recv() => {
                    println!("Yay! We have a captured request {:#?}!", captured);
                    assert!(captured.is_some());
                    break;
                }

                else => {
                    break;
                }
            }
        }

        drop(interceptor_stream);

        println!("I'm out of the loop");

        // TODO(alex) [high] 2022-12-14: Stuck here, as we never break the hyper task.
        println!("Hyper task {:#?}", hyper_task.await);
    }
}
