use bincode::Decode;
use mirrord_console::protocol::{Hello, Record};
use mirrord_intproxy_protocol::codec::AsyncDecoder;
use tokio::{
    io::BufReader,
    net::{TcpListener, TcpStream},
};
use tracing::metadata::LevelFilter;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

struct ConnectionWrapper {
    conn: BufReader<TcpStream>,
}

impl ConnectionWrapper {
    fn new(conn: TcpStream) -> Self {
        Self {
            conn: BufReader::new(conn),
        }
    }

    async fn next_message<T>(&mut self) -> Option<T>
    where
        T: Decode,
    {
        let mut decoder: AsyncDecoder<T, _> = AsyncDecoder::new(&mut self.conn);
        decoder
            .receive()
            .await
            .expect("failed to receive message from client")
    }
}

async fn serve_connection(conn: TcpStream) {
    let mut wrapper = ConnectionWrapper::new(conn);

    let client_info = match wrapper.next_message::<Hello>().await {
        Some(hello) => {
            tracing::info!("Client connected - process info {hello:?}");
            hello
        }
        None => {
            tracing::error!("Client disconnected without sending the `Hello` message");
            return;
        }
    };

    while let Some(record) = wrapper.next_message::<Record>().await {
        let logger = log::logger();

        logger.log(
            &log::Record::builder()
                .args(format_args!(
                    "pid {:?}: {:?}",
                    client_info.process_info.id, record.message
                ))
                .file(record.file.as_deref())
                .level(record.metadata.level.into())
                .module_path(record.module_path.as_deref())
                .target(&record.metadata.target)
                .line(record.line)
                .build(),
        );
    }

    tracing::info!("Client disconnected pid: {:?}", client_info.process_info.id);
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .compact(),
        )
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::TRACE.into())
                .from_env_lossy(),
        )
        .init();

    let listener = TcpListener::bind("0.0.0.0:11233")
        .await
        .expect("failed to setup TCP listener");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tracing::info!("accepted connection from {peer}");
                tokio::spawn(serve_connection(stream));
            }
            Err(e) => {
                tracing::error!("failed to accept connection: {e:?}");
            }
        }
    }
}
