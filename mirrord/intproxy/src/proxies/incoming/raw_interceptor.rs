use std::{
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    time,
};

use super::InterceptorMessageOut;
use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    codec::{self, CodecError},
};

#[derive(Error, Debug)]
pub enum RawInterceptorError {
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    #[error("{0}")]
    CodecError(#[from] CodecError),
}

pub struct RawInterceptor {
    remote_source: SocketAddr,
    local_destination: SocketAddr,
}

impl RawInterceptor {
    pub fn new(remote_source: SocketAddr, local_destination: SocketAddr) -> Self {
        Self {
            remote_source,
            local_destination,
        }
    }

    async fn connect_and_send_source(
        &self,
    ) -> Result<(OwnedWriteHalf, OwnedReadHalf), RawInterceptorError> {
        let stream = TcpStream::connect(self.local_destination).await?;

        let (mut codec_tx, codec_rx) = codec::make_async_framed::<SocketAddr, SocketAddr>(stream);
        codec_tx.send(&self.remote_source).await?;

        Ok((codec_tx.into_inner(), codec_rx.into_inner()))
    }
}

impl BackgroundTask for RawInterceptor {
    type Error = RawInterceptorError;
    type MessageIn = Vec<u8>;
    type MessageOut = InterceptorMessageOut;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let (mut write_half, mut read_half) = self.connect_and_send_source().await?;

        let mut buffer = vec![0; 1024];
        let mut remote_closed = false;

        loop {
            tokio::select! {
                biased;

                res = read_half.read(&mut buffer[..]) => match res {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => break Err(e.into()),
                    Ok(0) => break Ok(()),
                    Ok(read) => {
                        message_bus.send(InterceptorMessageOut::Bytes(buffer.get(..read).unwrap().to_vec())).await;
                    }
                },

                msg = message_bus.recv(), if !remote_closed => match msg {
                    None => remote_closed = true,
                    Some(data) => {
                        write_half.write_all(&data).await?;
                    }
                },

                _ = time::sleep(Duration::from_secs(1)), if remote_closed => break Ok(()),
            }
        }
    }
}
