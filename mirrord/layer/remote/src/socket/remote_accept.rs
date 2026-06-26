use std::{
    collections::HashMap,
    io::{self, Cursor, Write},
    net::SocketAddr,
    os::unix::{io::AsRawFd, net::UnixStream},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use mirrord_config::MIRRORD_AGENT_SIDECAR_REMOTE_ACCEPT_SOCKET;
use mirrord_intproxy_protocol::{
    RemoteAcceptResponse, RemoteAcceptedTransfer,
    codec::{SyncDecoder, SyncEncoder},
};
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};
use tracing::trace;

static NEXT_ACCEPT_ID: AtomicU64 = AtomicU64::new(1);
static ACCEPT_RECORDS: OnceLock<Mutex<HashMap<u64, RemoteAcceptedTransfer>>> = OnceLock::new();

fn accept_records() -> &'static Mutex<HashMap<u64, RemoteAcceptedTransfer>> {
    ACCEPT_RECORDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_accept_id() -> u64 {
    NEXT_ACCEPT_ID.fetch_add(1, Ordering::Relaxed)
}

fn remember_accept_record(transfer: &RemoteAcceptedTransfer) {
    accept_records()
        .lock()
        .expect("remote accept record lock failed")
        .insert(transfer.accept_id, transfer.clone());
}

pub(crate) fn accept_record(accept_id: u64) -> Option<RemoteAcceptedTransfer> {
    accept_records()
        .lock()
        .expect("remote accept record lock failed")
        .get(&accept_id)
        .cloned()
}

pub(crate) fn handoff_remote_accepted(
    listener_address: SocketAddr,
    local_address: SocketAddr,
    peer_address: SocketAddr,
    accepted_fd: i32,
) -> io::Result<RemoteAcceptResponse> {
    let socket_path =
        std::env::var(MIRRORD_AGENT_SIDECAR_REMOTE_ACCEPT_SOCKET).map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("missing sidecar remote-accept socket path: {error}"),
            )
        })?;

    let stream = UnixStream::connect(socket_path)?;
    let mut stream_reader = stream.try_clone()?;
    let transfer = RemoteAcceptedTransfer {
        accept_id: next_accept_id(),
        listener_address,
        local_address,
        peer_address,
    };

    remember_accept_record(&transfer);
    send_fd_frame(&stream, &transfer, accepted_fd)?;
    let response = receive_ack(&mut stream_reader)?;

    trace!(
        accept_id = transfer.accept_id,
        listener_address = %transfer.listener_address,
        peer_address = %transfer.peer_address,
        verdict = ?response.verdict,
        "completed remote accepted fd handoff"
    );

    Ok(response)
}

fn send_fd_frame(
    stream: &UnixStream,
    transfer: &RemoteAcceptedTransfer,
    accepted_fd: i32,
) -> io::Result<()> {
    let frame = encode_transfer_frame(transfer)?;
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
                "failed to slice remote-accept frame",
            )
        })?)?;
    }

    Ok(())
}

fn receive_ack(stream: &mut UnixStream) -> io::Result<RemoteAcceptResponse> {
    let mut decoder = SyncDecoder::new(stream.try_clone()?);
    decoder.receive().map_err(io::Error::other)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing remote-accept verdict",
        )
    })
}

fn encode_transfer_frame(value: &RemoteAcceptedTransfer) -> io::Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut encoder = SyncEncoder::new(cursor);
    encoder.send(value).map_err(io::Error::other)?;

    Ok(encoder.into_inner().into_inner())
}
