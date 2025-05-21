use std::ops::Not;

use bytes::BytesMut;
use http_body_util::BodyExt;
use hyper::{body::Incoming, upgrade::OnUpgrade};
use hyper_util::rt::TokioIo;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

use crate::incoming::{
    connection::{
        util::{AutoDropBroadcast, StealerSender},
        IncomingIO,
    },
    error::{ConnError, ResultExt, StealerDropped},
    IncomingStreamItem,
};

pub type UpgradeDataRx = mpsc::Receiver<Vec<u8>>;

/// Background task responsible for handling IO on stolen HTTP request.
pub struct StealTask {
    pub body_tail: Option<Incoming>,
    pub on_upgrade: OnUpgrade,
    pub upgrade_rx: oneshot::Receiver<Option<UpgradeDataRx>>,
    pub tx: StealerSender<IncomingStreamItem>,
    pub copy_tx: AutoDropBroadcast<IncomingStreamItem>,
}

impl StealTask {
    /// Runs this task until the request is finished.
    ///
    /// This method must ensure that the final [`IncomingStreamItem::Finished`] is always sent to
    /// the clients.
    pub async fn run(mut self) {
        let result: Result<(), ConnError> = try {
            self.handle_frames().await?;
            self.handle_upgrade().await?;
        };

        let _ = self
            .tx
            .send(IncomingStreamItem::Finished(result.clone()))
            .await;
        self.copy_tx
            .send(IncomingStreamItem::Finished(result.clone()));
    }

    /// Handles reading request body frames.
    async fn handle_frames(&mut self) -> Result<(), ConnError> {
        let Some(mut body) = self.body_tail.take() else {
            return Ok(());
        };

        while let Some(result) = body.frame().await {
            let frame = result.map_err_into(ConnError::IncomingHttpError)?;

            self.copy_tx.send(&frame);
            self.tx
                .send(IncomingStreamItem::Frame(frame.into()))
                .await?;
        }

        self.copy_tx.send(IncomingStreamItem::NoMoreFrames);
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

        let upgraded = (&mut self.on_upgrade)
            .await
            .map_err_into(ConnError::IncomingHttpError)?;

        let parts_upgraded = upgraded
            .downcast::<TokioIo<Box<dyn IncomingIO>>>()
            .expect("io type is known");

        if parts_upgraded.read_buf.is_empty().not() {
            self.copy_tx.send(parts_upgraded.read_buf.as_ref());
            self.tx
                .send(IncomingStreamItem::Data(parts_upgraded.read_buf.into()))
                .await?;
        }

        let mut upgraded = parts_upgraded.io.into_inner();
        let mut buffer = BytesMut::with_capacity(64 * 1024);

        let mut stealer_writes = true;
        let mut peer_writes = true;

        while stealer_writes || peer_writes {
            tokio::select! {
                result = data_rx.recv(), if stealer_writes => match result {
                    Some(data) => {
                        upgraded.write_all(&data).await.map_err_into(ConnError::IncomingUpgradedError)?;
                    }
                    None => {
                        stealer_writes = false;
                        upgraded.shutdown().await.map_err_into(ConnError::IncomingUpgradedError)?;
                    }
                },

                result = upgraded.read_buf(&mut buffer), if peer_writes => {
                    result.map_err_into(ConnError::IncomingUpgradedError)?;

                    if buffer.is_empty() {
                        peer_writes = false;
                        self.tx.send(IncomingStreamItem::NoMoreData).await?;
                        self.copy_tx.send(IncomingStreamItem::NoMoreData);
                    } else {
                        self.copy_tx.send(buffer.as_ref());
                        self.tx.send(IncomingStreamItem::Data(buffer.to_vec())).await?;
                        buffer.clear();
                    }
                },
            }
        }

        Ok(())
    }
}
