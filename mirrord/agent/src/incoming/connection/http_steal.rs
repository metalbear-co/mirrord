use std::ops::Not;

use bytes::BytesMut;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

use super::IncomingStreamItem;
use crate::{
    http::extract_requests::HttpUpgrade,
    incoming::{
        connection::{util::StealerSender, IncomingIO},
        error::{ConnError, StealerDropped},
    },
};

pub type UpgradeDataRx = mpsc::Receiver<Vec<u8>>;

/// Background task responsible for handling IO on a stolen HTTP request.
pub struct StealTask {
    /// Frames that we need to send to the stealing client.
    pub body_tail: Option<Incoming>,
    /// Extracted from the original request.
    pub on_upgrade: HttpUpgrade<TokioIo<Box<dyn IncomingIO>>>,
    /// Channel we use to receive an optional HTTP upgrade from the stealing client.
    ///
    /// When a value is received on this channel, this task assumes that the HTTP exchange is
    /// finished.
    pub upgrade_rx: oneshot::Receiver<Option<UpgradeDataRx>>,
    /// Channel we use to send data to the stealing client.
    pub tx: StealerSender<IncomingStreamItem>,
}

impl StealTask {
    /// Runs this task until the request is finished.
    ///
    /// This method must ensure that the final [`IncomingStreamItem::Finished`] is always sent to
    /// the client.
    pub async fn run(mut self) {
        let result: Result<(), ConnError> = try {
            self.handle_frames().await?;
            self.handle_upgrade().await?;
        };

        let _ = self.tx.send(IncomingStreamItem::Finished(result)).await;
    }

    /// Handles reading request body frames.
    ///
    /// In this task, we only handle body tail, i.e. frames that were not initially ready.
    async fn handle_frames(&mut self) -> Result<(), ConnError> {
        let Some(mut body) = self.body_tail.take() else {
            return Ok(());
        };

        while let Some(result) = body.frame().await {
            let frame = result.map_err(ConnError::IncomingHttpError)?;

            self.tx
                .send(IncomingStreamItem::Frame(frame.into()))
                .await?;
        }

        self.tx.send(IncomingStreamItem::NoMoreFrames).await?;

        Ok(())
    }

    /// Handles bidirectional data transfer after an HTTP upgrade.
    async fn handle_upgrade(&mut self) -> Result<(), ConnError> {
        let mut data_rx = match (&mut self.upgrade_rx).await {
            Ok(Some(data_rx)) => data_rx,
            Ok(None) => return Ok(()),
            Err(..) => return Err(StealerDropped.into()),
        };

        let (upgraded, read_buf) = (&mut self.on_upgrade)
            .await
            .map_err(ConnError::IncomingHttpError)?;

        if read_buf.is_empty().not() {
            self.tx
                .send(IncomingStreamItem::Data(read_buf.into()))
                .await?;
        }

        let mut upgraded = upgraded.into_inner();
        let mut buffer = BytesMut::with_capacity(64 * 1024);

        let mut stealer_writes = true;
        let mut peer_writes = true;

        while stealer_writes || peer_writes {
            tokio::select! {
                result = data_rx.recv(), if stealer_writes => match result {
                    Some(data) => {
                        upgraded.write_all(&data).await.map_err(ConnError::UpgradedError)?;
                    }
                    None => {
                        stealer_writes = false;
                        upgraded.shutdown().await.map_err(ConnError::UpgradedError)?;
                    }
                },

                result = upgraded.read_buf(&mut buffer), if peer_writes => {
                    result.map_err(ConnError::UpgradedError)?;

                    if buffer.is_empty() {
                        peer_writes = false;
                        self.tx.send(IncomingStreamItem::NoMoreData).await?;
                    } else {
                        self.tx.send(IncomingStreamItem::Data(buffer.to_vec())).await?;
                        buffer.clear();
                    }
                },
            }
        }

        Ok(())
    }
}
