use std::borrow::Cow;

use bytes::BytesMut;
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};

use crate::incoming::{
    connection::{
        util::{AutoDropBroadcast, StealerSender},
        ConnectionInfo, IncomingIO,
    },
    error::{ConnError, ResultExt},
    IncomingStreamItem,
};

/// Background task responsible for handling IO on redirected TCP connections.
pub struct TcpTask {
    pub incoming_io: Box<dyn IncomingIO>,
    pub destination: Destination,
    pub copy_tx: AutoDropBroadcast<IncomingStreamItem>,
}

impl TcpTask {
    /// Runs this task until the connection is closed.
    ///
    /// This method must ensure that the final [`IncomingStreamItem::Finished`] is always sent to
    /// the clients.
    pub async fn run(mut self) {
        let result = self.run_inner().await;
        if let Destination::StealingClient { data_tx, .. } = &self.destination {
            let _ = data_tx
                .send(IncomingStreamItem::Finished(result.clone()))
                .await;
        }
        self.copy_tx
            .send(IncomingStreamItem::Finished(result.clone()));
    }

    async fn run_inner(&mut self) -> Result<(), ConnError> {
        let mut source_writes = true;
        let mut destination_writes = true;
        let mut read_buf = BytesMut::with_capacity(64 * 1024);

        while source_writes || destination_writes {
            tokio::select! {
                result = self.incoming_io.read_buf(&mut read_buf), if source_writes => {
                    result.map_err_into(ConnError::IncomingTcpError)?;

                    if read_buf.is_empty() {
                        source_writes = false;

                        self.destination.shutdown().await?;
                        self.copy_tx.send(IncomingStreamItem::NoMoreData);

                        continue;
                    }

                    self.copy_tx.send(read_buf.as_ref());
                    self.destination.send(&read_buf).await?;

                    read_buf.clear();
                },

                recv_result = self.destination.recv(), if destination_writes => match recv_result? {
                    data if data.is_empty() => {
                        destination_writes = false;
                        self.incoming_io
                            .shutdown()
                            .await
                            .map_err_into(ConnError::IncomingTcpError)?;
                    },

                    data => {
                        self.incoming_io
                            .write_all(&data)
                            .await
                            .map_err_into(ConnError::IncomingTcpError)?;
                        self.incoming_io
                            .flush()
                            .await
                            .map_err_into(ConnError::IncomingTcpError)?;
                    }
                },
            }
        }

        Ok(())
    }
}

/// Destination of a redirected TCP connection,
/// used in the [`TcpTask`].
pub enum Destination {
    PassThrough {
        stream: MaybeTls,
        buffer: BytesMut,
    },
    StealingClient {
        data_tx: StealerSender<IncomingStreamItem>,
        data_rx: mpsc::Receiver<Vec<u8>>,
    },
}

impl Destination {
    pub async fn pass_through(info: &ConnectionInfo) -> Result<Self, ConnError> {
        let tcp_stream = TcpStream::connect(info.pass_through_address())
            .await
            .map_err_into(ConnError::TcpConnectError)?;

        match &info.tls_connector {
            Some(tls_connector) => {
                let stream = tls_connector
                    .connect(info.original_destination.ip(), None, tcp_stream)
                    .await
                    .map_err_into(ConnError::TlsConnectError)?;

                Ok(Self::PassThrough {
                    stream: MaybeTls::Tls(stream),
                    buffer: BytesMut::with_capacity(64 * 1024),
                })
            }

            None => Ok(Self::PassThrough {
                stream: MaybeTls::NoTls(tcp_stream),
                buffer: BytesMut::with_capacity(64 * 1024),
            }),
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), ConnError> {
        match self {
            Self::PassThrough { stream, .. } => stream
                .write_all(data)
                .await
                .map_err_into(ConnError::PassthroughTcpError),

            Self::StealingClient { data_tx, .. } => data_tx
                .send(IncomingStreamItem::Data(data.into()))
                .await
                .map_err(From::from),
        }
    }

    async fn recv(&mut self) -> Result<Cow<'_, [u8]>, ConnError> {
        match self {
            Self::PassThrough { stream, buffer } => {
                buffer.clear();
                stream
                    .read_buf(buffer)
                    .await
                    .map_err_into(ConnError::PassthroughTcpError)?;

                Ok(Cow::Borrowed(buffer))
            }

            Self::StealingClient { data_rx, .. } => {
                let data = data_rx.recv().await.unwrap_or_default();

                Ok(Cow::Owned(data))
            }
        }
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        match self {
            Self::PassThrough { stream, .. } => stream
                .shutdown()
                .await
                .map_err_into(ConnError::PassthroughTcpError),

            Self::StealingClient { data_tx, .. } => data_tx
                .send(IncomingStreamItem::NoMoreData)
                .await
                .map_err(From::from),
        }
    }
}
