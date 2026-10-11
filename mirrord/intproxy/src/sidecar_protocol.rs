//! Protocol between an internal proxy running in a sidecar container and the external proxy on
//! the container host.
//!
//! The external proxy relays the agent connection of the internal proxy, so most of what the two
//! exchange are `mirrord-protocol` messages. Wrapping them in frames of this protocol lets the
//! two proxies also exchange messages of their own, which the agent never sees.
//!
//! Only the direction from the internal proxy is framed. A stream starts with [`PREAMBLE`] and
//! continues with frames, each a tag byte followed by a payload:
//!
//! | tag             | payload                                     |
//! |-----------------|---------------------------------------------|
//! | [`AGENT_FRAME`] | one encoded [`ClientMessage`] for the agent |
//! | [`HOOKS_FRAME`] | one encoded [`HooksReport`]                 |
//!
//! The direction from the external proxy carries plain [`DaemonMessage`]s.
//!
//! The image of the sidecar is configurable, so the two proxies can come from different mirrord
//! versions, one of which does not know this protocol:
//! * The internal proxy uses it only when the connect info it was started with says so, which only
//!   a CLI whose external proxy understands it does.
//! * The external proxy recognizes an internal proxy that sends plain [`ClientMessage`]s by the
//!   missing [`PREAMBLE`].

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::{Decoder, Encoder, Framed};
use bincode::{Decode, Encode, error::DecodeError};
use bytes::{Buf, BufMut, BytesMut};
use futures::{Sink, SinkExt, Stream, StreamExt};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage, ProtocolCodec};
use mirrord_protocol_io::{AsyncIO, Client, Connection};
use tokio::sync::mpsc;
use tokio_util::sync::{PollSendError, PollSender};

/// Opens a stream that uses this protocol.
///
/// A `mirrord-protocol` message starts with the index of its enum variant, which bincode never
/// encodes with a leading `0xFF`, so no stream of plain [`ClientMessage`]s starts like this.
const PREAMBLE: [u8; 8] = *b"\xFFmirrord";

/// Tag of a frame that holds a [`ClientMessage`] to pass on to the agent.
const AGENT_FRAME: u8 = 0;

/// Tag of a frame that holds a [`HooksReport`].
const HOOKS_FRAME: u8 = 1;

/// Upper bound on the encoded size of a [`HooksReport`], far above that of the layer's whole
/// hook list. Decoding a report that claims to be larger fails instead of buffering it.
const HOOKS_REPORT_LIMIT: usize = 64 * 1024;

/// How many frames of one kind can wait to be written to the external proxy.
const FRAMES_CHANNEL_SIZE: usize = 64;

/// Which libc hooks one layer of a container session installed, and the platform it runs on.
///
/// The external proxy sends the analytics of the session, and runs on the container host,
/// whose platform can differ from the container's.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct HooksReport {
    /// CPU architecture, in the numeric form used in analytics.
    pub arch: u32,
    /// Major and minor version of the operating system, when known.
    pub os_version: Option<(u32, u32)>,
    /// Symbols whose hook was installed.
    pub installed: Vec<String>,
    /// Symbols whose hook could not be installed.
    pub failed: Vec<String>,
}

/// Message of an internal proxy to the external proxy.
#[derive(Debug, PartialEq, Eq)]
pub enum SidecarMessage {
    /// Message to pass on to the agent.
    Agent(ClientMessage),
    /// Report for the external proxy itself.
    HooksInstalled(HooksReport),
}

/// What the internal proxy writes to the stream.
enum Frame {
    /// A [`ClientMessage`] already encoded for the agent, as that is what [`Connection`] hands
    /// to its transport.
    Agent(Vec<u8>),
    Hooks(HooksReport),
}

/// Makes the agent connection of an internal proxy over `io`, a stream to the external proxy.
///
/// `sidecar_protocol` tells whether the external proxy expects this protocol or plain
/// `mirrord-protocol`. With this protocol, the returned sender delivers [`HooksReport`]s to the
/// external proxy.
pub fn connect<IO>(
    io: IO,
    sidecar_protocol: bool,
) -> (Connection<Client>, Option<mpsc::Sender<HooksReport>>)
where
    IO: AsyncIO + Unpin,
{
    if !sidecar_protocol {
        return (Connection::from_stream(io), None);
    }

    let (mut sink, messages) = Framed::new(io, InternalProxyCodec::default()).split();
    let (agent_frames_tx, mut agent_frames_rx) = mpsc::channel(FRAMES_CHANNEL_SIZE);
    let (reports_tx, mut reports_rx) = mpsc::channel(FRAMES_CHANNEL_SIZE);

    // The connection and the sender of reports both write to the stream, which this task owns
    // on their behalf. It lives as long as the connection.
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                biased;

                frame = agent_frames_rx.recv() => match frame {
                    Some(frame) => frame,
                    None => break,
                },
                Some(report) = reports_rx.recv() => Frame::Hooks(report),
            };

            if let Err(error) = sink.send(frame).await {
                tracing::error!(%error, "failed to send a frame to the external proxy");
                break;
            }
        }
    });

    let connection = Connection::from_channel(Transport {
        agent_frames: PollSender::new(agent_frames_tx),
        messages,
    });

    (connection, Some(reports_tx))
}

/// The two directions of a stream to the external proxy, as [`Connection`] takes them.
struct Transport<S> {
    /// To the task that writes to the stream.
    agent_frames: PollSender<Frame>,
    /// Read from the stream.
    messages: S,
}

impl<S> Transport<S> {
    fn closed(_: PollSendError<Frame>) -> io::Error {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "connection to the external proxy is closed",
        )
    }
}

impl<S> Stream for Transport<S>
where
    S: Stream<Item = io::Result<DaemonMessage>> + Unpin,
{
    type Item = io::Result<DaemonMessage>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().messages.poll_next_unpin(cx)
    }
}

impl<S: Unpin> Sink<Vec<u8>> for Transport<S> {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut()
            .agent_frames
            .poll_ready_unpin(cx)
            .map_err(Self::closed)
    }

    fn start_send(self: Pin<&mut Self>, client_message: Vec<u8>) -> io::Result<()> {
        self.get_mut()
            .agent_frames
            .start_send_unpin(Frame::Agent(client_message))
            .map_err(Self::closed)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut()
            .agent_frames
            .poll_flush_unpin(cx)
            .map_err(Self::closed)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut()
            .agent_frames
            .poll_close_unpin(cx)
            .map_err(Self::closed)
    }
}

/// Codec for the end of the internal proxy.
#[derive(Default)]
struct InternalProxyCodec {
    preamble_sent: bool,
}

impl Encoder<Frame> for InternalProxyCodec {
    type Error = io::Error;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> io::Result<()> {
        if !self.preamble_sent {
            dst.put_slice(&PREAMBLE);
            self.preamble_sent = true;
        }

        let (tag, payload) = match frame {
            Frame::Agent(client_message) => (AGENT_FRAME, client_message),
            Frame::Hooks(report) => (
                HOOKS_FRAME,
                bincode::encode_to_vec(report, bincode::config::standard())
                    .map_err(io::Error::other)?,
            ),
        };

        dst.reserve(payload.len() + 1);
        dst.put_u8(tag);
        dst.put_slice(&payload);

        Ok(())
    }
}

impl Decoder for InternalProxyCodec {
    type Item = DaemonMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<DaemonMessage>> {
        ProtocolCodec::<DaemonMessage, ()>::default().decode(src)
    }
}

/// What the internal proxy on the other end of a stream speaks.
#[derive(Default, Clone, Copy)]
enum Peer {
    /// Not known before the first bytes of the stream arrive.
    #[default]
    Unknown,
    /// Plain [`ClientMessage`]s.
    Plain,
    /// The protocol of this module.
    Sidecar,
}

/// Codec for the end of the external proxy.
#[derive(Default)]
pub struct ExternalProxyCodec {
    peer: Peer,
}

impl Decoder for ExternalProxyCodec {
    type Item = SidecarMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<SidecarMessage>> {
        if matches!(self.peer, Peer::Unknown) {
            if src
                .iter()
                .zip(PREAMBLE)
                .any(|(received, expected)| *received != expected)
            {
                self.peer = Peer::Plain;
            } else if src.len() < PREAMBLE.len() {
                return Ok(None);
            } else {
                src.advance(PREAMBLE.len());
                self.peer = Peer::Sidecar;
            }
        }

        let mut agent_codec = DaemonCodec::default();

        let message = match (self.peer, src.first()) {
            (Peer::Plain, _) => agent_codec.decode(src)?.map(SidecarMessage::Agent),
            (_, None) => None,
            (_, Some(&AGENT_FRAME)) => decode_payload(src, |payload| agent_codec.decode(payload))?
                .map(SidecarMessage::Agent),
            (_, Some(&HOOKS_FRAME)) => {
                decode_payload(src, decode_hooks_report)?.map(SidecarMessage::HooksInstalled)
            }
            (_, Some(tag)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown frame tag {tag} from the internal proxy"),
                ));
            }
        };

        Ok(message)
    }
}

/// Decodes the payload of the frame at the start of `src` with `decode`, which gets the bytes
/// that follow the tag. The frame is removed from `src` only once its whole payload is there.
fn decode_payload<T>(
    src: &mut BytesMut,
    decode: impl FnOnce(&mut BytesMut) -> io::Result<Option<T>>,
) -> io::Result<Option<T>> {
    let mut payload = src.split_off(1);
    let decoded = decode(&mut payload);
    if matches!(decoded, Ok(Some(_))) {
        src.clear();
    }
    src.unsplit(payload);

    decoded
}

fn decode_hooks_report(payload: &mut BytesMut) -> io::Result<Option<HooksReport>> {
    let config = bincode::config::standard().with_limit::<HOOKS_REPORT_LIMIT>();

    match bincode::decode_from_slice(payload.as_ref(), config) {
        Ok((report, consumed)) => {
            payload.advance(consumed);
            Ok(Some(report))
        }
        Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

impl Encoder<DaemonMessage> for ExternalProxyCodec {
    type Error = io::Error;

    fn encode(&mut self, message: DaemonMessage, dst: &mut BytesMut) -> io::Result<()> {
        DaemonCodec::default().encode(message, dst)
    }
}

#[cfg(test)]
mod tests {
    use actix_codec::{Decoder, Encoder, Framed};
    use bytes::BytesMut;
    use futures::{SinkExt, StreamExt};
    use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage, GetEnvVarsRequest, VERSION};

    use super::{
        ExternalProxyCodec, Frame, HooksReport, InternalProxyCodec, PREAMBLE, SidecarMessage,
    };

    fn client_messages() -> Vec<ClientMessage> {
        vec![
            ClientMessage::SwitchProtocolVersion(VERSION.clone()),
            ClientMessage::Ping,
            ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter: ["A".to_owned(), "B".to_owned()].into(),
                env_vars_select: ["*".to_owned()].into(),
            }),
            ClientMessage::Close,
        ]
    }

    fn encode_for_agent(message: ClientMessage) -> BytesMut {
        let mut encoded = BytesMut::new();
        ClientCodec::default()
            .encode(message, &mut encoded)
            .unwrap();
        encoded
    }

    /// Decodes `stream` as it arrives when split into chunks of `chunk_size` bytes.
    fn decode_in_chunks(stream: &[u8], chunk_size: usize) -> Vec<SidecarMessage> {
        let mut codec = ExternalProxyCodec::default();
        let mut buffer = BytesMut::new();
        let mut decoded = Vec::new();

        for chunk in stream.chunks(chunk_size) {
            buffer.extend_from_slice(chunk);
            while let Some(message) = codec.decode(&mut buffer).unwrap() {
                decoded.push(message);
            }
        }

        assert!(buffer.is_empty(), "undecoded bytes left: {buffer:?}");
        decoded
    }

    fn expected() -> Vec<SidecarMessage> {
        client_messages()
            .into_iter()
            .map(SidecarMessage::Agent)
            .collect()
    }

    fn hooks_report() -> HooksReport {
        HooksReport {
            arch: 2,
            os_version: Some((6, 8)),
            installed: vec!["open".to_owned(), "connect".to_owned()],
            failed: vec!["open64".to_owned()],
        }
    }

    #[test]
    fn external_proxy_decodes_sidecar_protocol() {
        let mut codec = InternalProxyCodec::default();
        let mut stream = BytesMut::new();
        let mut expected = Vec::new();
        for message in client_messages() {
            let frame = Frame::Agent(encode_for_agent(message.clone()).to_vec());
            codec.encode(frame, &mut stream).unwrap();
            expected.push(SidecarMessage::Agent(message));

            codec
                .encode(Frame::Hooks(hooks_report()), &mut stream)
                .unwrap();
            expected.push(SidecarMessage::HooksInstalled(hooks_report()));
        }

        assert!(stream.starts_with(&PREAMBLE));
        for chunk_size in [1, 2, 3, 7, PREAMBLE.len(), stream.len()] {
            assert_eq!(decode_in_chunks(&stream, chunk_size), expected);
        }
    }

    #[test]
    fn external_proxy_rejects_oversized_hooks_report() {
        let mut stream = BytesMut::new();
        let report = HooksReport {
            installed: vec!["a".repeat(super::HOOKS_REPORT_LIMIT)],
            ..hooks_report()
        };
        InternalProxyCodec::default()
            .encode(Frame::Hooks(report), &mut stream)
            .unwrap();

        // The claimed size is known from the first bytes of the report.
        stream.truncate(PREAMBLE.len() + 32);
        assert!(ExternalProxyCodec::default().decode(&mut stream).is_err());
    }

    /// Both kinds of frames of a connected internal proxy reach the external proxy, whose
    /// messages reach the internal proxy.
    #[tokio::test]
    async fn connection_carries_agent_messages_and_reports() {
        let (internal, external) = tokio::io::duplex(1024);
        let (connection, reports) = super::connect(internal, true);
        let mut connection = connection;
        let mut external = Framed::new(external, ExternalProxyCodec::default());

        connection.send(ClientMessage::Ping).await;
        assert_eq!(
            external.next().await.unwrap().unwrap(),
            SidecarMessage::Agent(ClientMessage::Ping),
        );

        reports.unwrap().send(hooks_report()).await.unwrap();
        assert_eq!(
            external.next().await.unwrap().unwrap(),
            SidecarMessage::HooksInstalled(hooks_report()),
        );

        external.send(DaemonMessage::Pong).await.unwrap();
        assert_eq!(connection.recv().await, Some(DaemonMessage::Pong));

        drop(connection);
        assert!(external.next().await.is_none());
    }

    #[test]
    fn external_proxy_decodes_plain_protocol() {
        let mut stream = BytesMut::new();
        for message in client_messages() {
            stream.unsplit(encode_for_agent(message));
        }

        for chunk_size in [1, 2, 3, 7, stream.len()] {
            assert_eq!(decode_in_chunks(&stream, chunk_size), expected());
        }
    }

    #[test]
    fn external_proxy_rejects_unknown_frame() {
        let mut buffer = BytesMut::from(&PREAMBLE[..]);
        buffer.extend_from_slice(&[200, 1, 2, 3]);

        assert!(ExternalProxyCodec::default().decode(&mut buffer).is_err());
    }
}
