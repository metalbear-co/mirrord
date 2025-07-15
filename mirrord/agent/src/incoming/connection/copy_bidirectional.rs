use std::{future::Future, ops::Not};

use bytes::{Bytes, BytesMut};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{broadcast, mpsc},
};

use crate::incoming::{ConnError, IncomingStreamItem};

/// Copies data bidirectionally between an incoming stream and an outgoing destination.
///
/// # Params
///
/// * `incoming` - an incoming data stream
/// * `outgoing` - an outgoing data destination (e.g. a stealing client or a passthrough connection)
/// * `ready_incoming_data` - incoming data that was already received and is ready to be sent to the
///   outgoing destination
pub async fn copy_bidirectional<I, O>(
    incoming: &mut I,
    outgoing: &mut O,
    ready_incoming_data: Bytes,
) -> Result<(), ConnError>
where
    I: AsyncRead + AsyncWrite + Unpin,
    O: OutgoingDestination,
{
    if ready_incoming_data.is_empty().not() {
        outgoing
            .send_data(CowBytes::Owned(ready_incoming_data))
            .await?;
    }

    let mut buffer = BytesMut::with_capacity(64 * 1024);

    let mut outgoing_writes = true;
    let mut incoming_writes = true;

    while outgoing_writes || incoming_writes {
        tokio::select! {
            result = outgoing.recv(), if outgoing_writes => {
                let data = result?;
                if data.as_ref().is_empty() {
                    outgoing_writes = false;
                    incoming.shutdown().await.map_err(From::from).map_err(ConnError::IncomingIoError)?;
                } else {
                    incoming.write_all(data.as_ref()).await.map_err(From::from).map_err(ConnError::IncomingIoError)?;
                }
            },

            result = incoming.read_buf(&mut buffer), if incoming_writes => {
                result.map_err(From::from).map_err(ConnError::IncomingIoError)?;

                if buffer.is_empty() {
                    incoming_writes = false;
                    outgoing.shutdown().await?;
                } else {
                    outgoing.send_data(CowBytes::Borrowed(buffer.as_ref())).await?;
                    buffer.clear();
                }
            },
        }
    }

    Ok(())
}

/// Custom [`std::borrow::Cow`] implementation that uses [`Bytes`] for the owned variant.
///
/// Allows for efficient handling of both owned and borrowed data (no unnecessary cloning).
pub enum CowBytes<'a> {
    Owned(Bytes),
    Borrowed(&'a [u8]),
}

impl AsRef<[u8]> for CowBytes<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            CowBytes::Owned(bytes) => bytes.as_ref(),
            CowBytes::Borrowed(slice) => slice,
        }
    }
}

/// An outgoing destination for incoming data.
///
/// E.g. [`StealingClient`] or [`PassthroughConnection`].
pub trait OutgoingDestination {
    /// Sends the data to the destination.
    fn send_data(&mut self, data: CowBytes<'_>) -> impl Future<Output = Result<(), ConnError>>;

    /// Shuts down writing to the destination.
    fn shutdown(&mut self) -> impl Future<Output = Result<(), ConnError>>;

    /// Receives data from the destination.
    ///
    /// Returning empty data here will be interpreted as a write shutdown from the destination.
    fn recv(&mut self) -> impl Future<Output = Result<CowBytes<'_>, ConnError>>;
}

/// [`OutgoingDestination`] implementation for a stealing client.
pub struct StealingClient {
    pub data_rx: mpsc::Receiver<Bytes>,
    pub data_tx: mpsc::Sender<IncomingStreamItem>,
    pub mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
}

impl OutgoingDestination for StealingClient {
    async fn recv(&mut self) -> Result<CowBytes<'_>, ConnError> {
        Ok(CowBytes::Owned(
            self.data_rx.recv().await.unwrap_or_default(),
        ))
    }

    async fn send_data(&mut self, data: CowBytes<'_>) -> Result<(), ConnError> {
        let item = match data {
            CowBytes::Owned(bytes) => IncomingStreamItem::Data(bytes),
            CowBytes::Borrowed(slice) => IncomingStreamItem::Data(slice.to_vec().into()),
        };
        let _ = self.mirror_data_tx.send(item.clone());
        self.data_tx
            .send(item)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        let _ = self.mirror_data_tx.send(IncomingStreamItem::NoMoreData);
        self.data_tx
            .send(IncomingStreamItem::NoMoreData)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }
}

/// [`OutgoingDestination`] implementation for a passthrough connection to the original destination.
pub struct PassthroughConnection<IO> {
    pub stream: IO,
    pub buffer: BytesMut,
    pub mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
}

impl<IO> OutgoingDestination for PassthroughConnection<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    async fn recv(&mut self) -> Result<CowBytes<'_>, ConnError> {
        self.buffer.clear();
        self.stream
            .read(&mut self.buffer)
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        Ok(CowBytes::Borrowed(self.buffer.as_ref()))
    }

    async fn send_data(&mut self, data: CowBytes<'_>) -> Result<(), ConnError> {
        let bytes = match &data {
            CowBytes::Owned(bytes) => bytes.clone(),
            CowBytes::Borrowed(slice) => slice.to_vec().into(),
        };
        let _ = self.mirror_data_tx.send(IncomingStreamItem::Data(bytes));
        self.stream
            .write_all(data.as_ref())
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        let _ = self.mirror_data_tx.send(IncomingStreamItem::NoMoreData);
        self.stream
            .shutdown()
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)
    }
}
