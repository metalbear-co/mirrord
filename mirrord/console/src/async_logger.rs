use drain::Watch;
use log::LevelFilter;
use mirrord_intproxy::codec::AsyncEncoder;
use tokio::{
    io::BufWriter,
    net::TcpStream,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::Result,
    protocol::{Hello, Metadata, Record},
};

struct LoggerTask {
    encoder: AsyncEncoder<Record, BufWriter<TcpStream>>,
    rx: Receiver<Record>,
    cancellation_token: CancellationToken,
}

impl LoggerTask {
    fn new(rx: Receiver<Record>, cancellation_token: CancellationToken, stream: TcpStream) -> Self {
        Self {
            encoder: AsyncEncoder::new(BufWriter::new(stream)),
            rx,
            cancellation_token,
        }
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => break,

                msg = self.rx.recv() => match msg {
                    None => break,
                    Some(record) => {
                        if let Err(e) = self.encoder.send(&record).await {
                            eprintln!("Error sending log message: {e:?}");
                            break;
                        }
                    }
                }
            }
        }
    }
}

pub struct AsyncConsoleLogger {
    tx: Sender<Record>,
}

impl log::Log for AsyncConsoleLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().contains("mirrord")
    }

    fn flush(&self) {}

    fn log(&self, record: &log::Record) {
        let msg = Record {
            metadata: Metadata {
                level: record.level().into(),
                target: record.target().to_string(),
            },
            message: record.args().to_string(),
            module_path: record.module_path().map(|s| s.to_string()),
            file: record.file().map(|s| s.to_string()),
            line: record.line(),
        };

        if self.tx.blocking_send(msg).is_err() {
            eprintln!("Error sending log message: channel closed");
        }
    }
}

/// Send hello message, containing information about the connected process.
async fn send_hello(stream: &mut TcpStream) -> Result<()> {
    let hello = Hello::from_env();

    let mut encoder: AsyncEncoder<Hello, &mut TcpStream> = AsyncEncoder::new(stream);
    encoder.send(&hello).await?;

    Ok(())
}

pub async fn init_async_logger(address: &str, watch: Watch, channel_size: usize) -> Result<()> {
    let mut stream = TcpStream::connect(address).await?;
    send_hello(&mut stream).await?;

    let (tx, rx) = mpsc::channel(channel_size);

    let logger = AsyncConsoleLogger { tx };
    log::set_boxed_logger(Box::new(logger)).map(|()| log::set_max_level(LevelFilter::Trace))?;

    let cancellation_token = CancellationToken::new();
    let task = LoggerTask::new(rx, cancellation_token.clone(), stream);
    tokio::spawn(watch.watch(Box::pin(task.run()), move |_| cancellation_token.cancel()));

    Ok(())
}
