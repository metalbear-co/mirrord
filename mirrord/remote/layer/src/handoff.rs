use std::{
    io::{self, Cursor, Write},
    net::SocketAddr,
    os::unix::{io::AsRawFd, net::UnixStream},
    sync::atomic::{AtomicU64, Ordering},
};

use mirrord_intproxy_protocol::codec::{SyncDecoder, SyncEncoder};
use mirrord_remote_layer_protocol::{
    CONNECTION_HANDOFF_SOCKET_ENV, ConnectionHandoffRequest, ConnectionHandoffResponse,
};
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};

use crate::error::Result;

static NEXT_CONNECTION_HANDOFF_ID: AtomicU64 = AtomicU64::new(1);

fn next_connection_handoff_id() -> u64 {
    NEXT_CONNECTION_HANDOFF_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn handoff_remote_connection(
    listener_address: SocketAddr,
    local_address: SocketAddr,
    peer_address: SocketAddr,
    accepted_fd: i32,
) -> Result<ConnectionHandoffResponse> {
    tracing::debug!(%listener_address, %local_address, %peer_address, accepted_fd, "remote-layer starting connection handoff");

    let socket_path = std::env::var(CONNECTION_HANDOFF_SOCKET_ENV)?;

    let stream = UnixStream::connect(socket_path)?;
    let mut stream_reader = stream.try_clone()?;
    let request = ConnectionHandoffRequest {
        accept_id: next_connection_handoff_id(),
        listener_address,
        local_address,
        peer_address,
    };

    send_connection_handoff_request(&stream, &request, accepted_fd)?;
    let response = receive_connection_handoff_response(&mut stream_reader)?;

    tracing::trace!(
        accept_id = request.accept_id,
        listener_address = %request.listener_address,
        peer_address = %request.peer_address,
        verdict = ?response.verdict,
        "remote-layer accept hook completed connection handoff negotiation"
    );

    Ok(response)
}

fn send_connection_handoff_request(
    stream: &UnixStream,
    request: &ConnectionHandoffRequest,
    accepted_fd: i32,
) -> Result<()> {
    tracing::trace!(
        accept_id = request.accept_id,
        listener_address = %request.listener_address,
        peer_address = %request.peer_address,
        accepted_fd,
        "remote-layer sending connection handoff request"
    );

    let frame = encode_connection_handoff_request(request)?;
    let iov = [std::io::IoSlice::new(&frame)];
    let rights = [accepted_fd];
    let written = sendmsg::<()>(
        stream.as_raw_fd(),
        &iov,
        &[ControlMessage::ScmRights(&rights)],
        MsgFlags::empty(),
        None,
    )?;

    if written < frame.len() {
        let mut stream = stream;
        stream.write_all(frame.get(written..).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "failed to slice connection-handoff frame",
            )
        })?)?;
    }

    Ok(())
}

fn receive_connection_handoff_response(
    stream: &mut UnixStream,
) -> Result<ConnectionHandoffResponse> {
    tracing::trace!("remote-layer waiting for connection handoff response");
    let mut decoder = SyncDecoder::<ConnectionHandoffResponse, _>::new(stream.try_clone()?);
    let response = decoder.receive()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing connection-handoff response",
        )
    })?;

    Ok(response)
}

fn encode_connection_handoff_request(value: &ConnectionHandoffRequest) -> Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut encoder = SyncEncoder::new(cursor);
    encoder.send(value)?;

    Ok(encoder.into_inner().into_inner())
}
