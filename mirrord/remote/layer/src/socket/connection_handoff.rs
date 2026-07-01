use std::{
    io::{self, Cursor, Write},
    net::SocketAddr,
    os::unix::{io::AsRawFd, net::UnixStream},
    sync::atomic::{AtomicU64, Ordering},
};

use mirrord_intproxy_protocol::codec::{SyncDecoder, SyncEncoder};
use mirrord_remote_layer_protocol::{
    AcceptHandoffRequest, AcceptHandoffResponse, CONNECTION_HANDOFF_SOCKET_ENV,
};
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};
use tracing::trace;

static NEXT_CONNECTION_HANDOFF_ID: AtomicU64 = AtomicU64::new(1);

fn next_connection_handoff_id() -> u64 {
    NEXT_CONNECTION_HANDOFF_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn handoff_remote_connection(
    listener_address: SocketAddr,
    local_address: SocketAddr,
    peer_address: SocketAddr,
    accepted_fd: i32,
) -> io::Result<AcceptHandoffResponse> {
    let socket_path = std::env::var(CONNECTION_HANDOFF_SOCKET_ENV).map_err(|error| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing sidecar connection-handoff socket path: {error}"),
        )
    })?;

    let stream = UnixStream::connect(socket_path)?;
    let mut stream_reader = stream.try_clone()?;
    let request = AcceptHandoffRequest {
        accept_id: next_connection_handoff_id(),
        listener_address,
        local_address,
        peer_address,
    };

    send_connection_handoff_request(&stream, &request, accepted_fd)?;
    let response = receive_connection_handoff_response(&mut stream_reader)?;

    trace!(
        accept_id = request.accept_id,
        listener_address = %request.listener_address,
        peer_address = %request.peer_address,
        verdict = ?response.verdict,
        "completed connection handoff for accepted fd"
    );

    Ok(response)
}

fn send_connection_handoff_request(
    stream: &UnixStream,
    request: &AcceptHandoffRequest,
    accepted_fd: i32,
) -> io::Result<()> {
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
) -> io::Result<AcceptHandoffResponse> {
    let mut decoder = SyncDecoder::new(stream.try_clone()?);
    decoder.receive().map_err(io::Error::other)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing connection-handoff response",
        )
    })
}

fn encode_connection_handoff_request(value: &AcceptHandoffRequest) -> io::Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut encoder = SyncEncoder::new(cursor);
    encoder.send(value).map_err(io::Error::other)?;

    Ok(encoder.into_inner().into_inner())
}
